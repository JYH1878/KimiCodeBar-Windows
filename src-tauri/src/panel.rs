use std::sync::atomic::{AtomicU64, Ordering};
use tauri::{AppHandle, LogicalSize, Manager, PhysicalPosition, Position, Size, WebviewWindow};

/// 面板边缘与托盘图标 / 屏幕边缘之间保留的间距（物理像素）。
const EDGE_MARGIN: i32 = 8;

/// 极简模式的面板紧凑逻辑高度（只显示页头 + 7天/5小时额度条 + 翻页圆点 + 底栏）。
/// 内容自适应生效后的兜底：前端尚未实测时用它做极简模式的初始高。
const MINIMAL_PANEL_HEIGHT: f64 = 350.0;

/// 内容自适应的最小窗口逻辑高：防极端内容（如只剩页头 + 底栏）把窗口压得失形。
const MIN_PANEL_HEIGHT: f64 = 200.0;

/// 内容驱动的高度缓动动画时长 / 帧间隔（开窗前的 fit 不动画，见 fit 的 animate 参数）
const RESIZE_ANIM_MS: u64 = 250;
const RESIZE_FRAME_MS: u64 = 16;
/// 高度动画序号（单调递增）：新动画顶掉进行中的旧动画（可中断，从实时高度起步）
static RESIZE_ANIM_SEQ: AtomicU64 = AtomicU64::new(0);

/// ease-out cubic 缓动：起步快、收尾稳
fn ease_out_cubic(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(3)
}

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
    // 开窗前校准必须瞬时（面板一出现就是对的尺寸，不播动画）
    fit_panel_to_screen(app, &window, tray, false);
    position_panel(app, &window, tray);
    let _ = window.show();
    let _ = window.set_focus();
}

/// 面板基准逻辑高：极简模式取紧凑高，普通模式取配置高。
fn base_panel_height(config_height: f64, minimal_mode: bool) -> f64 {
    if minimal_mode {
        MINIMAL_PANEL_HEIGHT
    } else {
        config_height
    }
}

/// 小屏兜底：面板实际可用的逻辑像素高度。
/// 显示器物理高按缩放比换算成逻辑高、减去边缘间距后，与配置高取小，避免矮屏把面板裁掉。
fn effective_panel_height(config_height: f64, monitor_height_px: u32, monitor_scale: f64) -> f64 {
    let cap = monitor_height_px as f64 / monitor_scale - (2 * EDGE_MARGIN) as f64 / monitor_scale;
    config_height.min(cap).max(1.0)
}

/// 每次显示面板前校准窗口尺寸：宽恒取配置值，高优先取前端实测内容高（AppState 里的
/// 面板内容高度，自适应主路径），未实测过时回退到基准高（极简紧凑高/配置高），
/// 并在显示器可用高度内取小（小屏压高）。
/// 全程只用逻辑像素 + 托盘所在显示器的缩放比：此时窗口可能还停在别的显示器上，
/// `window.scale_factor()` 是错的；也不把量到的窗口尺寸喂回去（outer_size 与
/// `set_size(Size::Physical)` 单位语义不一致会把窗口越撑越宽，面板两侧露白边）。
fn fit_panel_to_screen(app: &AppHandle, window: &WebviewWindow, tray: TrayRect, animate: bool) {
    let Some(conf) = app.config().app.windows.iter().find(|w| w.label == "main") else {
        return;
    };
    let Ok(Some(monitor)) = app.monitor_from_point(tray.x as f64, tray.y as f64) else {
        return; // 取不到显示器：保持配置尺寸，不压高
    };
    let scale = monitor.scale_factor();
    // 极简模式读当前设置（读失败按普通模式兜底，不压矮）
    let minimal_mode = kimicodebar::storage::load_settings()
        .map(|s| s.minimal_mode)
        .unwrap_or(false);
    // 内容自适应：前端实测内容高优先（含最小高保护），未实测回退基准高
    let base = app
        .state::<crate::commands::AppState>()
        .panel_content_height()
        .map(|h| h.max(MIN_PANEL_HEIGHT))
        .unwrap_or_else(|| base_panel_height(conf.height, minimal_mode));
    let target_h = effective_panel_height(base, monitor.size().height, scale);
    // 已接近目标尺寸就不动（当前尺寸按窗口自己所在显示器的缩放比换算）
    let cur_scale = window.scale_factor().unwrap_or(scale);
    if let Ok(inner) = window.inner_size() {
        let cur_w = inner.width as f64 / cur_scale;
        let cur_h = inner.height as f64 / cur_scale;
        if (cur_w - conf.width).abs() < 2.0 && (cur_h - target_h).abs() < 2.0 {
            return;
        }
    }
    // animate=false（开窗前校准）一次性到位；animate=true（开窗后内容驱动）走缓动动画
    if !animate {
        let _ = window.set_size(Size::Logical(LogicalSize::new(conf.width, target_h)));
        return;
    }
    animate_height_to(app, window, tray, conf.width, target_h);
}

/// 把窗口高度从当前实时值缓动到目标值（ease-out cubic 250ms，底边锚定每帧重定位）。
/// 新的调用顶掉进行中的动画（序号失效即退出，下一次从实时高度重新起步，可中断）；
/// 面板中途被隐藏则让出（下次 show 时 fit 会一次性校准）。
/// 尺寸语义遵守 fit 同款约定：只写逻辑像素，读值仅作起点快照、不喂回。
fn animate_height_to(
    app: &AppHandle,
    window: &WebviewWindow,
    tray: TrayRect,
    width: f64,
    target_h: f64,
) {
    let cur_scale = window.scale_factor().unwrap_or(1.0);
    let start_h = window
        .inner_size()
        .map(|s| s.height as f64 / cur_scale)
        .unwrap_or(target_h);
    // 起点即目标：不动画，宽度顺带兜底校准
    if (start_h - target_h).abs() < 2.0 {
        let _ = window.set_size(Size::Logical(LogicalSize::new(width, target_h)));
        return;
    }
    let seq = RESIZE_ANIM_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    let win = window.clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let start = std::time::Instant::now();
        loop {
            if RESIZE_ANIM_SEQ.load(Ordering::SeqCst) != seq || !win.is_visible().unwrap_or(false) {
                return;
            }
            let t = (start.elapsed().as_millis() as f64 / RESIZE_ANIM_MS as f64).min(1.0);
            let h = start_h + (target_h - start_h) * ease_out_cubic(t);
            let _ = win.set_size(Size::Logical(LogicalSize::new(width, h)));
            // 底边锚定：每帧按当前尺寸重算位置（视觉上从托盘处向上长/向下收）
            position_panel(&app, &win, tray);
            if t >= 1.0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(RESIZE_FRAME_MS)).await;
        }
    });
}

/// 设置变更/内容高度变化后即时重算面板尺寸并重定位（仅面板可见时）。
/// animate=true 时高度变化走 250ms 缓动动画（内容驱动的翻页收放），false 瞬时到位。
/// 尺寸与定位走与 show_panel 完全相同的链路：conf 逻辑像素 + 托盘所在显示器缩放比。
pub fn refit_open_panel(app: &AppHandle, animate: bool) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if !window.is_visible().unwrap_or(false) {
        return;
    }
    let Some(tray) = app
        .tray_by_id(crate::tray::TRAY_ID)
        .and_then(|tray| tray.rect().ok().flatten())
        .map(|rect| TrayRect::new(rect.position, rect.size))
    else {
        return; // 取不到托盘位置：不动，下次打开面板时自然会校准
    };
    fit_panel_to_screen(app, &window, tray, animate);
    // 高度变化后按托盘位置重定位（否则压矮后面板下沿悬空、恢复后可能出屏）；
    // animate=true 时此行是第 0 帧定位，后续帧由 animate_height_to 逐帧锚定
    position_panel(app, &window, tray);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 常规 1080p 屏（100% 缩放）：配置高放得下，不压
    #[test]
    fn keeps_config_height_on_normal_screen() {
        assert_eq!(effective_panel_height(830.0, 1080, 1.0), 830.0);
    }

    /// 矮屏（物理高 600px）：压到「屏高 - 2*EDGE_MARGIN」的逻辑高
    #[test]
    fn clamps_to_short_screen() {
        assert_eq!(effective_panel_height(830.0, 600, 1.0), (600 - 16) as f64);
    }

    /// 高缩放比先把显示器物理高换算成逻辑高再比较（150% + 1080px：逻辑可用 720-10.7 ≈ 709.3 → 压）
    #[test]
    fn converts_monitor_height_before_comparing() {
        let h = effective_panel_height(830.0, 1080, 1.5);
        assert!((h - (1080.0 / 1.5 - 16.0 / 1.5)).abs() < 1e-9);
    }

    /// 高缩放比大屏（150% + 2160px）：逻辑可用 ≈ 1429 > 830，不压
    #[test]
    fn keeps_height_on_hidpi_large_screen() {
        assert_eq!(effective_panel_height(830.0, 2160, 1.5), 830.0);
    }

    /// 非整数缩放（125% + 2160px）：逻辑可用 ≈ 1715 > 830，不压
    #[test]
    fn keeps_height_at_fractional_scale() {
        assert_eq!(effective_panel_height(830.0, 2160, 1.25), 830.0);
    }

    /// 极端小屏防御：上限为负也不返回 0
    #[test]
    fn never_returns_zero_on_tiny_screen() {
        assert_eq!(effective_panel_height(830.0, 8, 1.0), 1.0);
    }

    /// 极简模式基准高：开取紧凑高，关取配置高
    #[test]
    fn base_height_switches_on_minimal_mode() {
        assert_eq!(base_panel_height(830.0, true), MINIMAL_PANEL_HEIGHT);
        assert_eq!(base_panel_height(830.0, false), 830.0);
    }

    /// 紧凑高在常规 1080p 屏放得下（不压），切回普通模式恢复配置高
    #[test]
    fn compact_height_fits_normal_screen() {
        assert_eq!(
            effective_panel_height(base_panel_height(830.0, true), 1080, 1.0),
            MINIMAL_PANEL_HEIGHT
        );
        assert_eq!(
            effective_panel_height(base_panel_height(830.0, false), 1080, 1.0),
            830.0
        );
    }

    /// 紧凑高同样受矮屏压高规则约束（300px 屏：可用 284 < 420 → 压到 屏高-2*EDGE_MARGIN）
    #[test]
    fn compact_height_still_clamped_on_short_screen() {
        assert_eq!(
            effective_panel_height(base_panel_height(830.0, true), 300, 1.0),
            (300 - 16) as f64
        );
    }

    /// ease-out cubic：端点精确（0→0、1→1），区间内单调递增且始终快于线性（缓出特征）
    #[test]
    fn ease_out_cubic_endpoints_and_shape() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        let mut prev = 0.0;
        for i in 1..=10 {
            let t = i as f64 / 10.0;
            let v = ease_out_cubic(t);
            assert!(v > prev, "应单调递增: {prev} -> {v}");
            assert!(v >= t, "缓出曲线应不落后于线性: {v} >= {t}");
            prev = v;
        }
    }
}
