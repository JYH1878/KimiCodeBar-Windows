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
pub fn update_tray_state(app: &AppHandle, low_warning: bool, tooltip_extra: Option<String>) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let bytes: &[u8] = if low_warning { ICON_WARN } else { ICON_NORMAL };
    if let Ok(icon) = tauri::image::Image::from_bytes(bytes) {
        let _ = tray.set_icon(Some(icon));
    }
    let tooltip = format!("KimiCodeBar{}", tooltip_extra.unwrap_or_default());
    let _ = tray.set_tooltip(Some(&tooltip));
}
