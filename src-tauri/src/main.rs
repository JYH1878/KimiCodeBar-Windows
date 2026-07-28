#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cli;
mod commands;
mod diagnostics;
mod hotkey;
mod i18n;
mod logging;
mod panel;
mod polling;
mod tray;

use tauri::Manager;

fn main() {
    // CLI 拦截必须先于一切 Tauri / 日志初始化：单实例插件会把第二个实例
    // 吞成"唤起已有面板"，--status 一旦走进 Builder 就无法输出 JSON 退出
    if let Some(code) = cli::maybe_run() {
        std::process::exit(code);
    }

    tauri::Builder::default()
        // single-instance 需最先注册：第二个实例启动时把已有实例的主面板显示出来
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            panel::show_panel_for_second_instance(app);
        }))
        // 全局热键：面板可见则隐藏，不可见则按托盘定位显示（与左键点托盘一致）
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state != tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        return;
                    }
                    let visible = app
                        .get_webview_window("main")
                        .is_some_and(|w| w.is_visible().unwrap_or(false));
                    if visible {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.hide();
                        }
                    } else {
                        panel::show_panel_for_second_instance(app);
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_opener::init())
        // 自定义协议供面板背景图：大图（MB 级）走 IPC+CSS data URL 会断，
        // 协议零拷贝直出。处理器忽略请求路径、只服务 settings 里配置的那张图（无路径穿越面）
        .register_uri_scheme_protocol("kimibg", |_ctx, _req| {
            match kimicodebar::background::load() {
                Some((bytes, mime)) => tauri::http::Response::builder()
                    .header("Content-Type", mime)
                    // 同格式换图文件名不变，靠前端 URL 加版本 query 强制重拉，这里禁缓存兜底
                    .header("Cache-Control", "no-store")
                    .body(bytes)
                    .unwrap_or_else(|_| tauri::http::Response::new(Vec::new())),
                None => tauri::http::Response::builder()
                    .status(404)
                    .body(Vec::new())
                    .unwrap_or_else(|_| tauri::http::Response::new(Vec::new())),
            }
        })
        // 启动时从 cache.json 预热最近一次配额（断网/未刷新也能展示）
        .manage(commands::AppState::new())
        .invoke_handler(tauri::generate_handler![
            commands::get_panel_state,
            commands::get_usage_history,
            commands::get_local_usage,
            commands::refresh_now,
            commands::check_update,
            commands::open_settings,
            commands::get_settings,
            commands::save_settings,
            commands::pause_global_hotkey,
            commands::resume_global_hotkey,
            commands::set_background_image,
            commands::clear_background_image,
            commands::set_background_preset,
            commands::set_api_key,
            commands::clear_api_key,
            commands::get_credential_status,
            commands::set_web_token,
            commands::clear_web_token,
            commands::start_device_login,
            commands::cancel_device_login,
            commands::oauth_logout,
            commands::open_log_dir,
            commands::export_diagnostics,
            commands::export_usage_report,
        ])
        .setup(|app| {
            // 日志必须最先初始化：之后所有埋点才有着落；失败退回 stderr，不 panic
            logging::init();
            tracing::info!("KimiCodeBar 启动 v{}", env!("CARGO_PKG_VERSION"));

            tray::setup(app.handle())?;

            // 按设置注册全局热键；被占用只告警，不阻断启动
            {
                let settings = kimicodebar::storage::load_settings().unwrap_or_default();
                if let Err(e) = hotkey::apply(app.handle(), settings.hotkey.as_deref()) {
                    tracing::warn!("{e}");
                }
            }

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
