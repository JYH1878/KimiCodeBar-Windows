//! 后台定时轮询：按设置间隔刷新全部账号；某账号低额度由无到有时发一次系统通知
//! （文案带账号名）；某账号 5 小时窗口重置前 15 分钟内（且剩余量 > 0）提醒一次"建议用完"。
//!
//! 刷新模式（settings.adaptive_refresh，默认开）：
//! 自适应 = 近 10 分钟内有新 token 消耗（本地 wire.jsonl 扫描的 machine_last_event_at）按 1 分钟轮询，
//! 静默按用户配置间隔；固定 = 恒按用户配置间隔。

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use kimicodebar::local_usage;
use kimicodebar::quota::QuotaDetail;
use kimicodebar::storage;
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;
use tokio::time::{interval, MissedTickBehavior};

use crate::commands::{do_refresh, AccountPanel, AppState};
use kimicodebar::i18n;

/// 重置提醒提前量：进入重置前 15 分钟窗口才提醒
const RESET_REMIND_WINDOW_MIN: i64 = 15;
/// 活跃判定窗口：最近一次 token 消耗距今 ≤ 10 分钟记为活跃（恰好 10 分钟算活跃）
const ACTIVE_WINDOW_MS: i64 = 10 * 60 * 1000;
/// 自适应模式下活跃时的轮询间隔（秒）：写死 1 分钟
const ACTIVE_INTERVAL_SECS: u64 = 60;

/// 启动轮询任务：立即刷一次，之后按 settings.refresh_interval_min 分钟循环；
/// 自适应模式下活跃期收紧为 1 分钟（规则见 current_interval_secs）。
pub fn start(app: AppHandle) {
    // setup 在主线程执行、不在 tokio runtime 上下文里，
    // 必须用 tauri::async_runtime::spawn（内部惰性解析 runtime）
    tauri::async_runtime::spawn(async move {
        let mut current_secs = current_interval_secs().await;
        let mut timer = interval(Duration::from_secs(current_secs));
        // 单次刷新可能超过间隔时，错过的 tick 顺延而非补发，避免连续重刷
        timer.set_missed_tick_behavior(MissedTickBehavior::Delay);

        if crate::commands::total_silence() {
            tracing::info!(
                "KCB_TOTAL_SILENCE=1：轮询全静默模式（只做网络+落盘，tray/notify/emit 全跳过）"
            );
        }

        // 告警基线取启动时的内存态（各账号 cache-<id>.json 预热）：已处于低额的账号
        // 不重复提醒，只在某账号"由正常变低额"的跳变瞬间通知一次（按账号 id 跟踪）
        let mut prev_low: HashMap<String, bool> = app
            .state::<AppState>()
            .snapshot()
            .accounts
            .iter()
            .map(|a| (a.account.id.clone(), a.low_warning))
            .collect();

        // 5h 重置提醒去重：各账号已提醒过的重置时刻。轮询任务是唯一读写方，
        // 用循环局部变量即可（进程内记忆，重启后允许重发），无需 Mutex/static
        let mut last_reminded: HashMap<String, DateTime<Utc>> = HashMap::new();

        // 取证：轮询轮次序号（探针行带号，便于与踢人钟表时刻对齐）
        let mut round: u64 = 0;

        loop {
            // tokio interval 首个 tick 立即完成：启动即刷一次
            timer.tick().await;
            round += 1;
            let silence = crate::commands::total_silence();

            let panel = do_refresh(&app).await;
            let multi = panel.accounts.len() > 1;

            // 低额跳变通知：逐账号比较（拉取失败的账号 low_warning 恒为 false，不会触发）
            let mut low_actions: Vec<String> = Vec::new();
            for account in &panel.accounts {
                let was_low = prev_low.get(&account.account.id).copied().unwrap_or(false);
                if account.low_warning && !was_low {
                    let action = notify_low_warning(&app, account, multi, silence);
                    low_actions.push(format!("{}:{action}", account.account.name));
                }
            }
            // 重建基线：收敛掉已删除账号的条目，不留残留
            prev_low = panel
                .accounts
                .iter()
                .map(|a| (a.account.id.clone(), a.low_warning))
                .collect();

            // 5h 窗口重置前提醒：逐账号检查，且只在该账号刷新成功（error 为空）后，
            // 避免拿陈旧缓存误报
            let mut reset_actions: Vec<String> = Vec::new();
            for account in &panel.accounts {
                if account.error.is_some() {
                    continue;
                }
                let last = last_reminded.get(&account.account.id).copied();
                let (reminded, action) = notify_reset_reminder(&app, account, last, multi, silence);
                if let Some(reset_time) = reminded {
                    last_reminded.insert(account.account.id.clone(), reset_time);
                }
                // 常态动作（每轮都 not-due / DeepSeek 无配额）不进探针行，只留有意义事件
                if action != "not-due" && action != "no-quota" {
                    reset_actions.push(format!("{}:{action}", account.account.name));
                }
            }
            // 收敛已删除账号的去重条目
            last_reminded.retain(|id, _| panel.accounts.iter().any(|a| &a.account.id == id));

            // 取证日志（2026-09-08 验收后按拍板降级：只在动作实际发送或全静默模式下打，
            // 不再每轮一行）：汇总本轮 tray/notify/emit 动作 + 全屏守卫判定过程
            //（QUNS 原始值 + 前台窗口几何），与被踢钟表时刻对照即可定位「踢人瞬间谁在碰系统」
            let tray_action = crate::tray::take_last_tray_action().unwrap_or(if silence {
                "skipped-silence"
            } else {
                "not-run"
            });
            let any_sent = tray_action == "sent"
                || low_actions.iter().any(|a| a.ends_with(":sent"))
                || reset_actions.iter().any(|a| a.ends_with(":sent"));
            if any_sent || silence {
                let probe = kimicodebar::fullscreen::probe();
                tracing::info!(
                    "轮询探针 #{round}: tray={tray_action} emit={} notify_low=[{}] notify_reset=[{}] quns={:?} heuristic={} guard={} silence={silence} 前台={:?}",
                    if silence { "skipped-silence" } else { "sent" },
                    low_actions.join(","),
                    reset_actions.join(","),
                    probe.quns,
                    probe.heuristic,
                    probe.guard,
                    probe.foreground,
                );
            }

            // 设置页改了间隔 / 自适应活跃度翻转：重建 interval
            // （下一次 tick 立即触发，顺带马上刷一次，活跃期加密即时生效）
            let secs = current_interval_secs().await;
            if secs != current_secs {
                current_secs = secs;
                timer = interval(Duration::from_secs(secs));
                timer.set_missed_tick_behavior(MissedTickBehavior::Delay);
            }
        }
    });
}

/// low_warn_enabled 且该账号配额存在时发系统通知，正文为各窗口剩余百分比（语言随设置）；
/// 多账号时正文前缀账号名（"工作号 · 7天剩余 8%"），单账号只给摘要。
/// 返回本轮动作（埋点探针行用）：sent / skipped-silence / skipped-fullscreen /
/// disabled / no-quota / empty-summary
fn notify_low_warning(
    app: &AppHandle,
    account: &AccountPanel,
    multi: bool,
    silence: bool,
) -> &'static str {
    // 全静默取证开关：整次跳过，连系统通知 API 都不碰
    if silence {
        return "skipped-silence";
    }
    // 全屏守卫：游戏/演示全屏时静默（错过不补发——调用方随后的 prev_low 基线重建照常，
    // 本轮低额不会在退出全屏后重发）
    if kimicodebar::fullscreen::fullscreen_app_active() {
        return "skipped-fullscreen";
    }
    let settings = storage::load_settings().unwrap_or_default();
    if !settings.low_warn_enabled {
        return "disabled";
    }
    let Some(quota) = &account.quota else {
        return "no-quota";
    };
    let lang = i18n::resolve(settings.language.as_deref());
    let summary = i18n::quota_summary(lang, quota);
    if summary.is_empty() {
        return "empty-summary";
    }
    let body = with_account_name(multi, &account.account.name, &summary);
    tracing::info!(
        "notification.show 调用，caller=polling::notify_low_warning 账号={}",
        account.account.name
    );
    let _ = app
        .notification()
        .builder()
        .title(i18n::low_warning_title(lang))
        .body(i18n::low_warning_body(lang, &body))
        .show();
    "sent"
}

/// 多账号时给通知正文加账号名前缀（"·" 分隔，语言中立）；单账号原样返回
fn with_account_name(multi: bool, name: &str, body: &str) -> String {
    if multi {
        format!("{name} · {body}")
    } else {
        body.to_string()
    }
}

/// 当前应使用的轮询间隔（秒）：固定模式直接用用户配置；
/// 自适应模式按本地用量活跃度在 1 分钟与用户配置间切换。
/// 用量扫描自带 180s 进程内节流（local_usage::scan），1 分钟 tick 下不会每轮都读盘；
/// 扫描线程 panic 按空结果（静默）容忍，轮询主循环不受影响
async fn current_interval_secs() -> u64 {
    let settings = storage::load_settings().unwrap_or_default();
    let user_secs = settings.refresh_interval_secs();
    let active = if settings.adaptive_refresh {
        let view = tokio::task::spawn_blocking(local_usage::scan)
            .await
            .unwrap_or_default();
        usage_active(Utc::now().timestamp_millis(), view.machine_last_event_at)
    } else {
        false
    };
    poll_interval_secs(settings.adaptive_refresh, active, user_secs)
}

/// 活跃判定纯函数：最近一次 usage.record 事件距今 ≤ 10 分钟为活跃（恰好 10 分钟算活跃）。
/// None（从未扫到消耗）按静默；事件时间戳在未来（时钟回拨/写入方时钟偏快）差值为负，
/// 同样 ≤ 窗口按活跃——刚烧过 token 多刷几次无害
fn usage_active(now_ms: i64, last_event_ms: Option<i64>) -> bool {
    match last_event_ms {
        Some(ts) => now_ms - ts <= ACTIVE_WINDOW_MS,
        None => false,
    }
}

/// 轮询间隔选择纯函数：自适应模式且活跃 → 1 分钟；其余 → 用户配置间隔
fn poll_interval_secs(adaptive: bool, active: bool, user_secs: u64) -> u64 {
    if adaptive && active {
        ACTIVE_INTERVAL_SECS
    } else {
        user_secs
    }
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

/// 该账号 5h 窗口重置前提醒：low_warn_enabled 开着、5h 窗口剩余量 > 0（已烧完没必要提醒）
/// 且进入重置前 15 分钟窗口时，发系统通知；多账号时正文前缀账号名。
/// 返回（本次提醒针对的重置时刻，调用方用于去重；未提醒为 None）+
/// （本轮动作，埋点探针行用）：sent / skipped-silence / skipped-fullscreen /
/// disabled / no-quota / drained / no-reset / not-due
fn notify_reset_reminder(
    app: &AppHandle,
    account: &AccountPanel,
    last_reminded: Option<DateTime<Utc>>,
    multi: bool,
    silence: bool,
) -> (Option<DateTime<Utc>>, &'static str) {
    // 与低额度预警共用同一个通知总开关，不新增设置项
    let settings = storage::load_settings().unwrap_or_default();
    if !settings.low_warn_enabled {
        return (None, "disabled");
    }
    // 全静默取证开关：整次跳过，连系统通知 API 都不碰
    if silence {
        return (None, "skipped-silence");
    }
    // 全屏守卫：游戏/演示全屏时静默。返回 None 不记去重（不挂账）：
    // 退出全屏时若仍在 15 分钟窗口内，下一轮自然提醒；错过窗口则不补发
    if kimicodebar::fullscreen::fullscreen_app_active() {
        return (None, "skipped-fullscreen");
    }
    // 仅 5 小时窗口：7 天窗口周期太长，"用完"语义弱，不提醒
    let five_hour: &QuotaDetail = match account.quota.as_ref().and_then(|q| q.five_hour.as_ref()) {
        Some(detail) => detail,
        None => return (None, "no-quota"),
    };
    if five_hour.remaining <= 0.0 {
        return (None, "drained");
    }
    let Some(reset_time) = five_hour.reset_time else {
        return (None, "no-reset");
    };
    let now = Utc::now();
    if !reset_remind_due(now, reset_time, last_reminded) {
        return (None, "not-due");
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
    let body = with_account_name(multi, &account.account.name, &body);
    tracing::info!(
        "notification.show 调用，caller=polling::notify_reset_reminder 账号={}",
        account.account.name
    );
    let _ = app
        .notification()
        .builder()
        .title(i18n::reset_reminder_title(lang))
        .body(body)
        .show();
    // 无论系统通知是否真正弹出都记为已提醒：通知服务异常时不应每个 tick 重试
    (Some(reset_time), "sent")
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

    // ---- 多账号通知正文：账号名前缀 ----

    #[test]
    fn account_name_prefixed_only_when_multi() {
        // 多账号：前缀账号名消歧（GOAL 拍板"文案带账号名"）
        assert_eq!(
            with_account_name(true, "工作号", "7天剩余 8%"),
            "工作号 · 7天剩余 8%"
        );
        // 单账号：无前缀（无可消歧对象，保持旧版文案）
        assert_eq!(
            with_account_name(false, "账号 1", "7天剩余 8%"),
            "7天剩余 8%"
        );
    }

    // ---- 自适应刷新：活跃判定与间隔选择 ----

    #[test]
    fn adaptive_refresh_active_within_10_minutes() {
        let now_ms = at(1_000_000).timestamp_millis();
        // 9 分 59 秒前有消耗：活跃
        assert!(usage_active(now_ms, Some(now_ms - (9 * 60 + 59) * 1000)));
        // 事件时间戳在未来（时钟回拨）：差值为负，按活跃
        assert!(usage_active(now_ms, Some(now_ms + 60_000)));
    }

    #[test]
    fn adaptive_refresh_silent_beyond_10_minutes_or_no_events() {
        let now_ms = at(1_000_000).timestamp_millis();
        // 10 分 零 1 秒前的消耗：静默
        assert!(!usage_active(now_ms, Some(now_ms - (10 * 60 + 1) * 1000)));
        // 从未扫到消耗：静默
        assert!(!usage_active(now_ms, None));
    }

    #[test]
    fn adaptive_refresh_exactly_10_minutes_counts_active() {
        // 恰好 10 分钟归活跃侧（与 reset_remind_due"恰好 15 分钟算在内"同惯例：
        // "近 10 分钟内"含端点；偏向活跃只是多刷一次，代价低）
        let now_ms = at(1_000_000).timestamp_millis();
        assert!(usage_active(now_ms, Some(now_ms - 10 * 60 * 1000)));
    }

    #[test]
    fn adaptive_refresh_interval_choice() {
        // 自适应 + 活跃 → 写死 1 分钟
        assert_eq!(poll_interval_secs(true, true, 300), ACTIVE_INTERVAL_SECS);
        // 自适应 + 静默 → 用户配置间隔
        assert_eq!(poll_interval_secs(true, false, 300), 300);
        // 固定模式：无论活跃与否都按用户配置
        assert_eq!(poll_interval_secs(false, true, 300), 300);
        assert_eq!(poll_interval_secs(false, false, 300), 300);
    }
}
