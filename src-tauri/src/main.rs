#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod panel;
mod polling;
mod tray;

use tauri::Manager;

fn main() {
    tauri::Builder::default()
        // single-instance 需最先注册：第二个实例启动时把已有实例的主面板显示出来
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            panel::show_panel_for_second_instance(app);
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_opener::init())
        // 启动时从 cache.json 预热最近一次配额（断网/未刷新也能展示）
        .manage(commands::AppState::new())
        .invoke_handler(tauri::generate_handler![
            commands::get_panel_state,
            commands::refresh_now,
            commands::check_update,
            commands::open_settings,
            commands::get_settings,
            commands::save_settings,
            commands::set_api_key,
            commands::clear_api_key,
            commands::get_credential_status,
            commands::start_device_login,
            commands::cancel_device_login,
            commands::oauth_logout,
        ])
        .setup(|app| {
            tray::setup(app.handle())?;

            // 以系统实际自启状态为准回写设置（用户可能在系统设置里手动关过）
            {
                use tauri_plugin_autostart::ManagerExt;
                if let Ok(enabled) = app.autolaunch().is_enabled() {
                    let mut settings = kimicodebar::storage::load_settings().unwrap_or_default();
                    if settings.autostart != enabled {
                        settings.autostart = enabled;
                        let _ = kimicodebar::storage::save_settings(&settings);
                    }
                }
            }

            // 后台轮询：立即刷一次，之后按设置间隔循环
            polling::start(app.handle().clone());

            // 主面板失焦（点击到面板外）时自动隐藏
            if let Some(main_window) = app.get_webview_window("main") {
                let window = main_window.clone();
                main_window.on_window_event(move |event| {
                    if let tauri::WindowEvent::Focused(false) = event {
                        let _ = window.hide();
                    }
                });
            }

            // 设置窗口点关闭时只隐藏不销毁，保证能从托盘菜单再次打开
            if let Some(settings_window) = app.get_webview_window("settings") {
                let window = settings_window.clone();
                settings_window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                });
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running KimiCodeBar");
}
