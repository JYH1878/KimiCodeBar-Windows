use tauri::{AppHandle, Manager, PhysicalPosition, Position, Size, WebviewWindow};

/// 面板边缘与托盘图标 / 屏幕边缘之间保留的间距（物理像素）。
const EDGE_MARGIN: i32 = 8;

/// 托盘图标的物理像素区域（由 tray-icon 事件的 rect 转换而来）。
#[derive(Clone, Copy)]
pub struct TrayRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl TrayRect {
    /// 将 tray-icon 事件里的 position/size（可能是物理或逻辑坐标）统一换算成物理像素。
    pub fn new(position: Position, size: Size) -> Self {
        let (x, y) = match position {
            Position::Physical(p) => (p.x, p.y),
            Position::Logical(l) => (l.x.round() as i32, l.y.round() as i32),
        };
        let (width, height) = match size {
            Size::Physical(s) => (s.width as i32, s.height as i32),
            Size::Logical(s) => (s.width.round() as i32, s.height.round() as i32),
        };
        TrayRect {
            x,
            y,
            width,
            height,
        }
    }
}

/// 左键点击托盘图标：切换主面板显隐；显示前先定位到托盘图标上方。
pub fn toggle_panel(app: &AppHandle, tray: TrayRect) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        let _ = window.hide();
    } else {
        show_panel(app, tray);
    }
}

/// 重新定位并显示主面板（托盘菜单“刷新”的占位行为）。
pub fn show_panel(app: &AppHandle, tray: TrayRect) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    position_panel(app, &window, tray);
    let _ = window.show();
    let _ = window.set_focus();
}

/// 第二个实例启动时显示主面板：优先按托盘图标当前位置定位，
/// 取不到托盘 rect 时退化为仅 show + focus。
pub fn show_panel_for_second_instance(app: &AppHandle) {
    let tray_rect = app
        .tray_by_id(crate::tray::TRAY_ID)
        .and_then(|tray| tray.rect().ok().flatten())
        .map(|rect| TrayRect::new(rect.position, rect.size));
    match tray_rect {
        Some(tray) => show_panel(app, tray),
        None => {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }
    }
}

/// 把面板定位到托盘图标上方、水平居中对齐，并裁剪到所在显示器范围内。
fn position_panel(app: &AppHandle, window: &WebviewWindow, tray: TrayRect) {
    let size = window.outer_size().unwrap_or_default();
    let (win_w, win_h) = (size.width as i32, size.height as i32);

    let mut x = tray.x + tray.width / 2 - win_w / 2;
    let mut y = tray.y - win_h - EDGE_MARGIN;

    if let Ok(Some(monitor)) = app.monitor_from_point(tray.x as f64, tray.y as f64) {
        let mon_pos = monitor.position();
        let mon_size = monitor.size();
        let (mon_x, mon_y) = (mon_pos.x, mon_pos.y);
        let (mon_w, mon_h) = (mon_size.width as i32, mon_size.height as i32);

        // 托盘图标位于屏幕上半部（任务栏在顶部）时，改为放到图标下方
        if y < mon_y + EDGE_MARGIN {
            y = tray.y + tray.height + EDGE_MARGIN;
        }

        let min_x = mon_x + EDGE_MARGIN;
        let max_x = (mon_x + mon_w - win_w - EDGE_MARGIN).max(min_x);
        let min_y = mon_y + EDGE_MARGIN;
        let max_y = (mon_y + mon_h - win_h - EDGE_MARGIN).max(min_y);
        x = x.clamp(min_x, max_x);
        y = y.clamp(min_y, max_y);
    }

    let _ = window.set_position(Position::Physical(PhysicalPosition::new(x, y)));
}
