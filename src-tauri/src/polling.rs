//! 后台定时轮询：按设置间隔刷新配额，低额度由无到有时发系统通知；
//! 5 小时窗口重置前 15 分钟内（且剩余量 > 0）提醒一次"建议用完"。

use std::time::Duration;

use chrono::{DateTime, Utc};
use kimicodebar::quota::QuotaDetail;
use kimicodebar::storage;
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;
use tokio::time::{interval, MissedTickBehavior};

use crate::commands::{do_refresh, AppState, PanelState};
use crate::i18n;

/// 重置提醒提前量：进入重置前 15 分钟窗口才提醒
const RESET_REMIND_WINDOW_MIN: i64 = 15;

/// 启动轮询任务：立即刷一次，之后按 settings.refresh_interval_min 分钟循环。
pub fn start(app: AppHandle) {
    // setup 在主线程执行、不在 tokio runtime 上下文里，
    // 必须用 tauri::async_runtime::spawn（内部惰性解析 runtime）
    tauri::async_runtime::spawn(async move {
        let mut current_secs = load_interval_secs();
        let mut timer = interval(Duration::from_secs(current_secs));
        // 单次刷新可能超过间隔时，错过的 tick 顺延而非补发，避免连续重刷
        timer.set_missed_tick_behavior(MissedTickBehavior::Delay);

        // 告警基线取启动时的内存态（cache.json 预热）：已处于低额不重复提醒，
        // 只在"由正常变低额"的跳变瞬间通知一次
        let mut prev_low = app.state::<AppState>().snapshot().low_warning;

        // 5h 重置提醒去重：已提醒过的重置时刻。轮询任务是唯一读写方，
        // 用循环局部变量即可（进程内记忆，重启后允许重发），无需 Mutex/static
        let mut last_reminded: Option<DateTime<Utc>> = None;

        loop {
            // tokio interval 首个 tick 立即完成：启动即刷一次
            timer.tick().await;

            let panel = do_refresh(&app).await;

            if panel.low_warning && !prev_low {
                notify_low_warning(&app, &panel);
            }
            prev_low = panel.low_warning;

            // 5h 窗口重置前提醒：只在刷新成功（error 为空）后检查，避免拿陈旧缓存误报
            if panel.error.is_none() {
                if let Some(reset_time) = notify_reset_reminder(&app, &panel, last_reminded) {
                    last_reminded = Some(reset_time);
                }
            }

            // 设置页改了间隔：重建 interval（下一次 tick 立即触发，顺带马上刷一次）
            let secs = load_interval_secs();
            if secs != current_secs {
                current_secs = secs;
                timer = interval(Duration::from_secs(secs));
                timer.set_missed_tick_behavior(MissedTickBehavior::Delay);
            }
        }
    });
}

/// low_warn_enabled 且配额存在时发系统通知，正文为各窗口剩余百分比（语言随设置）
fn notify_low_warning(app: &AppHandle, panel: &crate::commands::PanelState) {
    let settings = storage::load_settings().unwrap_or_default();
    if !settings.low_warn_enabled {
        return;
    }
    let Some(quota) = &panel.quota else {
        return;
    };
    let lang = i18n::resolve(settings.language.as_deref());
    let summary = i18n::quota_summary(lang, quota);
    if summary.is_empty() {
        return;
    }
    let _ = app
        .notification()
        .builder()
        .title(i18n::low_warning_title(lang))
        .body(i18n::low_warning_body(lang, &summary))
        .show();
}

fn load_interval_secs() -> u64 {
    storage::load_settings()
        .unwrap_or_default()
        .refresh_interval_secs()
}

/// 是否到达重置提醒时机：重置时刻在未来 0–15 分钟内（恰好 15 分钟算在内，
/// 已过期/恰好到点不算），且该重置时刻尚未提醒过。
/// 纯函数，时间全部入参化以便单测。
fn reset_remind_due(
    now: DateTime<Utc>,
    reset_time: DateTime<Utc>,
    last_reminded: Option<DateTime<Utc>>,
) -> bool {
    if last_reminded == Some(reset_time) {
        return false;
    }
    now < reset_time && reset_time - now <= chrono::Duration::minutes(RESET_REMIND_WINDOW_MIN)
}

/// 5h 窗口重置前提醒：low_warn_enabled 开着、5h 窗口剩余量 > 0（已烧完没必要提醒）
/// 且进入重置前 15 分钟窗口时，发系统通知。
/// 返回本次提醒针对的重置时刻（调用方用于去重）；未提醒返回 None。
fn notify_reset_reminder(
    app: &AppHandle,
    panel: &PanelState,
    last_reminded: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    // 与低额度预警共用同一个通知总开关，不新增设置项
    let settings = storage::load_settings().unwrap_or_default();
    if !settings.low_warn_enabled {
        return None;
    }
    // 仅 5 小时窗口：7 天窗口周期太长，"用完"语义弱，不提醒
    let five_hour: &QuotaDetail = panel.quota.as_ref()?.five_hour.as_ref()?;
    if five_hour.remaining <= 0.0 {
        return None;
    }
    let reset_time = five_hour.reset_time?;
    let now = Utc::now();
    if !reset_remind_due(now, reset_time, last_reminded) {
        return None;
    }

    // 剩余分钟数四舍五入，至少显示 1（如 14.9 分钟 → 15，30 秒 → 1 而非 0）
    let mins = ((reset_time - now).num_seconds() as f64 / 60.0)
        .round()
        .max(1.0) as i64;
    let lang = i18n::resolve(settings.language.as_deref());
    let body = i18n::reset_reminder_body(
        lang,
        five_hour.remaining,
        five_hour.limit,
        mins,
        &five_hour.reset_time_text(),
    );
    let _ = app
        .notification()
        .builder()
        .title(i18n::reset_reminder_title(lang))
        .body(body)
        .show();
    // 无论系统通知是否真正弹出都记为已提醒：通知服务异常时不应每个 tick 重试
    Some(reset_time)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    #[test]
    fn due_exactly_15_minutes_before() {
        // 恰好 15 分钟：在提醒窗口内
        let now = at(1_000_000);
        let reset = now + Duration::minutes(15);
        assert!(reset_remind_due(now, reset, None));
    }

    #[test]
    fn due_14_9_minutes_before() {
        // 14.9 分钟：在窗口内
        let now = at(1_000_000);
        let reset = now + Duration::seconds(14 * 60 + 54);
        assert!(reset_remind_due(now, reset, None));
    }

    #[test]
    fn not_due_beyond_15_minutes() {
        // 超过 15 分钟：还没到提醒时机
        let now = at(1_000_000);
        let reset = now + Duration::minutes(15) + Duration::seconds(1);
        assert!(!reset_remind_due(now, reset, None));
    }

    #[test]
    fn not_due_when_reset_passed_or_now() {
        // 已过期不提醒；恰好到点（差值为 0）也不算"即将"重置
        let now = at(1_000_000);
        assert!(!reset_remind_due(now, now - Duration::seconds(1), None));
        assert!(!reset_remind_due(now, now, None));
    }

    #[test]
    fn not_due_when_same_reset_already_reminded() {
        // 同一重置时刻已提醒过：不重复提醒
        let now = at(1_000_000);
        let reset = now + Duration::minutes(10);
        assert!(!reset_remind_due(now, reset, Some(reset)));
    }

    #[test]
    fn due_again_for_new_reset_time() {
        // 提醒过的是旧时刻，进入窗口的是新重置时刻：仍需提醒
        let now = at(1_000_000);
        let old_reset = now - Duration::hours(5);
        let new_reset = now + Duration::minutes(10);
        assert!(reset_remind_due(now, new_reset, Some(old_reset)));
    }
}
