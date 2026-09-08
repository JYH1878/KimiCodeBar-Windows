use std::sync::Mutex;

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};

use crate::commands;
use crate::panel::{self, TrayRect};

pub const TRAY_ID: &str = "main-tray";

/// 常规 / 低额度预警 两套托盘图标（编译期嵌入）
const ICON_NORMAL: &[u8] = include_bytes!("../icons/tray-normal.png");
const ICON_WARN: &[u8] = include_bytes!("../icons/tray-warn.png");

/// 差分闸门：上次**实际发送**到系统 shell 的 (低额告警, tooltip 全文)；None = 启动后尚未发过。
/// 轮询每轮都调 update_tray_state，tooltip 含百分比几乎每轮都变，是唯一每轮必碰
/// Shell_NotifyIcon 的动作（任务栏闪烁/全屏踢出元凶）——无差分就零系统调用。
/// 全屏守卫跳过时**不落缓存**：退出全屏后的下一轮必不匹配，自然补发一次。
/// 反向保证：low_warning 翻转必改元组首元素，闸门吞不掉换红。
static LAST_SENT: Mutex<Option<(bool, String)>> = Mutex::new(None);

/// 埋点取证（临时代码，验收后删）：最近一次 update_tray_state 的动作，
/// polling 每轮取走汇总进探针行。注意：托盘菜单手动刷新也会写它，
/// 其动作会被归到下一轮探针行（单航班保证不交叉，归因偏差可接受）。
static LAST_TRAY_ACTION: Mutex<Option<&'static str>> = Mutex::new(None);

/// 记录本轮托盘动作（sent / skipped-diff / skipped-fullscreen / skipped-no-tray）
fn record_action(action: &'static str) {
    *LAST_TRAY_ACTION.lock().unwrap() = Some(action);
}

/// 取走最近一次托盘动作（取走后清空；None = 上轮轮询未触达 update_tray_state）
pub fn take_last_tray_action() -> Option<&'static str> {
    LAST_TRAY_ACTION.lock().unwrap().take()
}

/// 创建系统托盘图标：左键切换主面板，右键弹出菜单（刷新 / 设置 / 退出）。
pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let refresh = MenuItem::with_id(app, "refresh", "刷新", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "设置", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&refresh, &settings, &quit])?;

    TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("KimiCodeBar")
        .icon(tauri::image::Image::from_bytes(ICON_NORMAL)?)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } = event
            {
                let app = tray.app_handle();
                // 面板将由隐藏变显示：数据陈旧（>60s）或无缓存时后台刷新
                if let Some(window) = app.get_webview_window("main") {
                    if !window.is_visible().unwrap_or(false) {
                        commands::refresh_if_stale(app);
                    }
                }
                let tray_rect = TrayRect::new(rect.position, rect.size);
                // 埋点取证：panel::toggle_panel（panel.rs）内含 window.show/set_focus，此处记调用方
                tracing::info!(
                    "[埋点] 托盘左键点击 → panel::toggle_panel（内含 window.show/set_focus）"
                );
                panel::toggle_panel(app, tray_rect);
            }
        })
        .on_menu_event(|app, event| match event.id().as_ref() {
            "refresh" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    commands::do_refresh(&app).await;
                });
            }
            "settings" => {
                if let Some(window) = app.get_webview_window("settings") {
                    tracing::info!(
                        "[埋点] window.show+set_focus 调用，caller=tray::menu(settings)"
                    );
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;

    Ok(())
}

/// 按告警状态切换托盘图标，并更新 tooltip（"KimiCodeBar" + 可选的额度摘要行）。
/// tooltip_extra 形如 "\n7天剩余 87% · 5h剩余 36%"（英文 "\n7D left 87% · 5H left 36%"，
/// 由 do_refresh 按语言设置组装）。
///
/// 两道静默闸门（全屏不闪任务栏）：
/// ① 全屏守卫——全屏应用（游戏/演示）活跃时整次跳过，连 Shell_NotifyIcon 都不碰，
///    且不同步差分缓存（退出全屏后下一轮自然补发）；
/// ② 差分闸门——与上次实际发送一致时零系统调用（见 LAST_SENT）。
pub fn update_tray_state(app: &AppHandle, low_warning: bool, tooltip_extra: Option<String>) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        record_action("skipped-no-tray");
        return;
    };
    if kimicodebar::fullscreen::fullscreen_app_active() {
        record_action("skipped-fullscreen");
        tracing::info!(
            "[埋点] 全屏守卫命中：本轮托盘更新整次跳过（零 Shell_NotifyIcon，不落差分缓存）"
        );
        return;
    }
    let tooltip = format!("KimiCodeBar{}", tooltip_extra.unwrap_or_default());
    if already_sent(low_warning, &tooltip) {
        record_action("skipped-diff");
        return;
    }
    let bytes: &[u8] = if low_warning { ICON_WARN } else { ICON_NORMAL };
    if let Ok(icon) = tauri::image::Image::from_bytes(bytes) {
        let _ = tray.set_icon(Some(icon));
    }
    let _ = tray.set_tooltip(Some(&tooltip));
    tracing::info!("[埋点] tray set_icon+set_tooltip 实际发送（low_warning={low_warning}），caller=tray::update_tray_state");
    mark_sent(low_warning, tooltip);
    record_action("sent");
}

/// 差分比对：与上次实际发送完全一致 → true（本次可整段跳过）
fn already_sent(low_warning: bool, tooltip: &str) -> bool {
    let last = LAST_SENT.lock().unwrap();
    matches!(&*last, Some((lw, tt)) if *lw == low_warning && tt == tooltip)
}

/// 记录本次已实际发送的状态（set_icon/set_tooltip 失败也记：托盘操作本就 best-effort，
/// 记了失败态下轮不再重发，不记则 shell 异常时每轮空转重试）
fn mark_sent(low_warning: bool, tooltip: String) {
    *LAST_SENT.lock().unwrap() = Some((low_warning, tooltip));
}
