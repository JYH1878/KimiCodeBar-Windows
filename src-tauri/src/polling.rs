//! 后台定时轮询：按设置间隔刷新配额，低额度由无到有时发系统通知。

use std::time::Duration;

use kimicodebar::storage;
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;
use tokio::time::{interval, MissedTickBehavior};

use crate::commands::{do_refresh, quota_summary, AppState};

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

        loop {
            // tokio interval 首个 tick 立即完成：启动即刷一次
            timer.tick().await;

            let panel = do_refresh(&app).await;

            if panel.low_warning && !prev_low {
                notify_low_warning(&app, &panel);
            }
            prev_low = panel.low_warning;

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

/// low_warn_enabled 且配额存在时发系统通知，正文为各窗口剩余百分比
fn notify_low_warning(app: &AppHandle, panel: &crate::commands::PanelState) {
    let settings = storage::load_settings().unwrap_or_default();
    if !settings.low_warn_enabled {
        return;
    }
    let Some(quota) = &panel.quota else {
        return;
    };
    let summary = quota_summary(quota);
    if summary.is_empty() {
        return;
    }
    let _ = app
        .notification()
        .builder()
        .title("KimiCodeBar 额度预警")
        .body(summary)
        .show();
}

fn load_interval_secs() -> u64 {
    storage::load_settings()
        .unwrap_or_default()
        .refresh_interval_secs()
}
