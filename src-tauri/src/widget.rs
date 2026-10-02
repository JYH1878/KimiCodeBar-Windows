//! 桌面悬浮球（issue #58）：常驻桌面的 5 小时/7 天额度双环球。
//!
//! 形态与交互（任务书拍板）：
//! - 双环球 = 外环 7 天（weekly）、内环 5 小时（five_hour），中心数字为当前口径剩余百分比；
//! - 可拖拽（系统 startDragging），拖拽静止 300ms 后前端调 `widget_drag_ended`，
//!   后端按窗口与所在显示器工作区的关系判定贴边：贴左右竖边自动收缩成 12×88 细条
//!   （hover 展开回 96×96），自由悬浮保持 96×96；位置与贴边三字段落盘；
//! - 全屏应用活跃时自动隐藏（复用 fullscreen 双传感器守卫），退出恢复显示；
//! - 窗口懒创建懒销毁：`widget_enabled` 开才建（启动 / save_settings 后 reconcile），
//!   关即销毁，保低内存卖点。
//!
//! 坐标铁律（panel.rs 教训）：一律逻辑像素，所在显示器 scale_factor 换算只在读
//! 系统值（work_area / outer_position）出物理坐标时使用，禁止把物理回读值直接喂回
//! set_size / set_position（单位语义不一致会把窗口越撑越大）。

use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Position, Size, WebviewWindow,
};

use kimicodebar::storage;

use crate::commands::{AppState, PanelState};
/// 窗口 label（capabilities/default.json 的 windows 数组与前端按它索引）
pub const WIDGET_LABEL: &str = "widget";
/// 自由态窗口逻辑尺寸（球 76px 居中其中）
pub const FREE_SIZE: (f64, f64) = (96.0, 96.0);
/// 贴边细条窗口逻辑尺寸（前端渲染竖向两段色条）
pub const DOCK_SIZE: (f64, f64) = (12.0, 88.0);
/// 贴边判定阈值（逻辑像素）：窗口左/右边缘与所在显示器工作区对应边缘距离 ≤ 12 判贴边
pub const DOCK_EDGE_THRESHOLD: f64 = 12.0;
/// 默认位内缩（逻辑像素）：主屏工作区右下角向内缩 24
pub const DEFAULT_MARGIN: f64 = 24.0;
/// 全屏守卫探测周期（秒）
const FULLSCREEN_GUARD_SECS: u64 = 2;
/// 守卫代次：每次创建窗口 +1，旧守卫发现代次不符自动退出（快速开关不叠双守卫）
static WIDGET_GUARD_SEQ: AtomicU64 = AtomicU64::new(0);

/// 悬浮球展示数据（与 src/types.ts 的 WidgetState 一一对应，snake_case 序列化；
/// pct 均为剩余 0–100 语义，None = 该窗口无数据；余额类账号（DeepSeek）无
/// 5h/7d 窗口，改用 balance* 三字段展示）
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WidgetState {
    /// 是否有可展示的账号数据（false 时前端渲染灰球 "--"）
    pub has_data: bool,
    /// 展示账号的名称（无数据为空串）
    pub account_name: String,
    /// 5 小时窗口剩余百分比
    pub five_hour_pct: Option<f64>,
    /// 7 天窗口剩余百分比
    pub weekly_pct: Option<f64>,
    /// 中心数字（按 widget_center_metric 口径解析；无数据为 None）
    pub center_pct: Option<f64>,
    /// 余额类账号的余额金额（quota 类账号为 None）
    pub balance: Option<f64>,
    /// 余额币种（"CNY"/"USD" 等；仅余额类账号有值）
    pub balance_currency: Option<String>,
    /// 余额健康度：余额 / deepseek_warn_threshold × 100（100 = 正好在告警线，
    /// 越大越充裕）；告警线未配置（≤0）或取不到余额为 None。环填充取它的一半，
    /// 颜色带 200/100（绿 ≥2 倍告警线 / 琥珀 1–2 倍 / 红 < 1 倍）
    pub balance_pct: Option<f64>,
}

/// 参与悬浮球账号选择的账号快照（与 PanelState 解耦：纯函数只吃快照，单测不必构造完整状态）。
/// pct 均为剩余 0–100；余额类账号（DeepSeek）走 balance/balance_pct 两字段
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetAccountSnapshot {
    pub id: String,
    pub name: String,
    pub five_hour_pct: Option<f64>,
    pub weekly_pct: Option<f64>,
    /// 余额金额（quota 类为 None）
    pub balance: Option<f64>,
    /// 余额币种（quota 类为 None）
    pub balance_currency: Option<String>,
    /// 余额健康度（余额/告警线×100；告警线未配置或取不到余额为 None）
    pub balance_pct: Option<f64>,
}

impl WidgetAccountSnapshot {
    /// 该账号最紧张的窗口剩余（5h 与 7d 中较低者；都无数据为 None）
    fn tightest_pct(&self) -> Option<f64> {
        match (self.five_hour_pct, self.weekly_pct) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// 该账号是否有可展示的数据（配额窗口或余额任一有值）
    fn has_data(&self) -> bool {
        self.tightest_pct().is_some() || self.balance_pct.is_some()
    }
}

/// 悬浮球账号选择（纯函数）：
/// - 指定 widget_account_id 且该账号有可展示数据（配额或余额）→ 它；
/// - 否则自动：先在有配额数据的账号里取最紧张（tightest_pct 最低）者，并列取列表顺序靠前；
///   一个配额账号都没有时，退回余额账号里健康度最低（balance_pct 最小）者；
/// - 指定账号不存在/无数据、或全部账号都无数据 → None。
///
/// 配额与余额不同量纲，自动档不混算（余额类永远在没有配额账号时才自动顶上）。
pub fn select_widget_account<'a>(
    snapshots: &'a [WidgetAccountSnapshot],
    widget_account_id: Option<&str>,
) -> Option<&'a WidgetAccountSnapshot> {
    if let Some(id) = widget_account_id {
        let picked = snapshots
            .iter()
            .find(|s| s.id == id)
            .filter(|s| s.has_data());
        if let Some(s) = picked {
            return Some(s);
        }
    }
    let tightest_quota = snapshots
        .iter()
        .filter(|s| s.tightest_pct().is_some())
        .min_by(|a, b| {
            let pa = a.tightest_pct().unwrap_or(100.0);
            let pb = b.tightest_pct().unwrap_or(100.0);
            pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
        });
    if tightest_quota.is_some() {
        return tightest_quota;
    }
    snapshots
        .iter()
        .filter(|s| s.balance_pct.is_some())
        .min_by(|a, b| {
            let pa = a.balance_pct.unwrap_or(f64::MAX);
            let pb = b.balance_pct.unwrap_or(f64::MAX);
            pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
        })
}

/// 中心数字口径解析（纯函数，任务书 1b）：metric 选定（"five_hour"/"weekly"）且该项
/// 有值 → 该项；其余情况（None/"auto"/未知值/选定项无数据）回落 auto =
/// 已有项里剩余最低者；两项都无 → None
pub fn widget_center_pct(
    five_hour_pct: Option<f64>,
    weekly_pct: Option<f64>,
    metric: Option<&str>,
) -> Option<f64> {
    match metric {
        Some("five_hour") if five_hour_pct.is_some() => return five_hour_pct,
        Some("weekly") if weekly_pct.is_some() => return weekly_pct,
        _ => {}
    }
    match (five_hour_pct, weekly_pct) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// 贴边判定（纯函数，任务书 1c，逻辑像素）：窗口矩形 (x, y, w, h) 与所在显示器
/// 工作区矩形 (x, y, w, h)，窗口左/右「外侧边」与工作区对应边缘的距离 ≤
/// DOCK_EDGE_THRESHOLD 判为贴该边。**越出边缘（负距离）一律算贴**：用户按住球往边上推，
/// 窗口会跟着抓点越出屏幕边缘最多一个球宽（抓点偏移）——只认 ±阈值 会让「推到最边」
/// 这个最自然的动作永远判不到（2026-10-02 实机复现：抓球心推到极左 = 越出 46px → 拒绝）；
/// 两边都命中（屏极窄）取距离更近的一边，相等取左；其余 → None（自由悬浮）
pub fn detect_dock_edge(
    win: (f64, f64, f64, f64),
    work_area: (f64, f64, f64, f64),
) -> Option<&'static str> {
    let (wx, _, ww, _) = win;
    let (ax, _, aw, _) = work_area;
    let left_gap = wx - ax;
    let right_gap = (ax + aw) - (wx + ww);
    let tol = DOCK_EDGE_THRESHOLD;
    let left_hit = left_gap <= tol;
    let right_hit = right_gap <= tol;
    match (left_hit, right_hit) {
        (true, true) if left_gap.abs() <= right_gap.abs() => Some("left"),
        (true, true) => Some("right"),
        (true, false) => Some("left"),
        (false, true) => Some("right"),
        (false, false) => None,
    }
}

/// 点是否落在矩形内（纯函数，物理像素；右/下边界取开区间）。
/// 悬浮球「指针还在不在窗口里」判定用（收细条前复查真实光标位置）
pub(crate) fn point_in_rect(p: (i32, i32), r: (i32, i32, i32, i32)) -> bool {
    p.0 >= r.0 && p.0 < r.2 && p.1 >= r.1 && p.1 < r.3
}

/// 光标当前是否仍在窗口矩形内（物理像素：Tauri 的 outer_* 与 GetCursorPos 同为物理坐标）。
/// 用途：WebView2 在窗口缩放/重排瞬间会合成一次 mouseleave（指针其实没动），
/// 若直接收细条就是「hover 展开即收回」闪一下——收之前用真实光标位置复查。
/// 任一 API 失败按「不在窗内」（fail-open：允许正常收起，宁可多收一次不卡住展开态）
#[cfg(windows)]
fn cursor_inside_window(window: &WebviewWindow) -> bool {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

    let Ok(pos) = window.outer_position() else {
        return false;
    };
    let Ok(size) = window.outer_size() else {
        return false;
    };
    // SAFETY：GetCursorPos 只向本地变量写出一个点，调用同步返回
    let mut p = POINT { x: 0, y: 0 };
    if unsafe { GetCursorPos(&mut p) } == 0 {
        return false;
    }
    point_in_rect(
        (p.x, p.y),
        (
            pos.x,
            pos.y,
            pos.x.saturating_add(size.width as i32),
            pos.y.saturating_add(size.height as i32),
        ),
    )
}

#[cfg(not(windows))]
fn cursor_inside_window(_window: &WebviewWindow) -> bool {
    false
}

/// 余额健康度（纯函数）：余额 / 告警线 × 100（100 = 正好在告警线）。
/// 告警线未配置（≤0）按 200（充裕，绿）处理；余额取不到为 None
pub(crate) fn balance_pct(balance: Option<f64>, threshold: f64) -> Option<f64> {
    let b = balance?;
    if threshold <= 0.0 {
        return Some(200.0);
    }
    Some((b / threshold * 100.0).max(0.0))
}

/// 由面板状态 + 设置算出悬浮球展示数据（get_widget_state / widget-updated 共用）。
/// 无可展示账号时返回占位态（has_data=false，前端渲染灰球 "--"）
pub fn compute_widget_state(panel: &PanelState, settings: &storage::Settings) -> WidgetState {
    let snapshots: Vec<WidgetAccountSnapshot> = panel
        .accounts
        .iter()
        .map(|a| {
            let balance = a.deepseek_balance.as_ref().map(|b| b.total_balance);
            WidgetAccountSnapshot {
                id: a.account.id.clone(),
                name: a.account.name.clone(),
                five_hour_pct: a
                    .quota
                    .as_ref()
                    .and_then(|q| q.five_hour.as_ref())
                    .map(|d| d.percent_remaining),
                weekly_pct: a
                    .quota
                    .as_ref()
                    .and_then(|q| q.weekly.as_ref())
                    .map(|d| d.percent_remaining),
                balance,
                balance_currency: a.deepseek_balance.as_ref().map(|b| b.currency.clone()),
                balance_pct: if a.quota.is_some() {
                    None // 有额度窗口的账号走配额口径，不与余额混算
                } else {
                    balance_pct(balance, settings.deepseek_warn_threshold)
                },
            }
        })
        .collect();
    match select_widget_account(&snapshots, settings.widget_account_id.as_deref()) {
        Some(s) => WidgetState {
            has_data: true,
            account_name: s.name.clone(),
            five_hour_pct: s.five_hour_pct,
            weekly_pct: s.weekly_pct,
            center_pct: widget_center_pct(
                s.five_hour_pct,
                s.weekly_pct,
                settings.widget_center_metric.as_deref(),
            ),
            balance: s.balance,
            balance_currency: s.balance_currency.clone(),
            balance_pct: s.balance_pct,
        },
        None => WidgetState {
            has_data: false,
            account_name: String::new(),
            five_hour_pct: None,
            weekly_pct: None,
            center_pct: None,
            balance: None,
            balance_currency: None,
            balance_pct: None,
        },
    }
}

/// 广播 widget-updated（do_refresh 两处 emit quota-updated 旁与账号变更后调用）。
/// 球窗口不存在时跳过（前端不在，省一次序列化）
pub fn emit_widget_state(app: &AppHandle, panel: &PanelState) {
    if app.get_webview_window(WIDGET_LABEL).is_none() {
        return;
    }
    let settings = storage::load_settings().unwrap_or_default();
    let state = compute_widget_state(panel, &settings);
    let _ = app.emit("widget-updated", &state);
}

/// 按设置收敛悬浮球窗口的存在性（启动 / save_settings 后调用）：
/// 开 → 懒创建；关 → 销毁；状态未变 → 无操作
pub fn reconcile(app: &AppHandle) {
    let enabled = storage::load_settings()
        .map(|s| s.widget_enabled)
        .unwrap_or(false);
    match (enabled, app.get_webview_window(WIDGET_LABEL).is_some()) {
        (true, false) => {
            if let Err(e) = create_widget_window(app) {
                tracing::warn!("创建悬浮球窗口失败: {e}");
            }
        }
        (false, true) => destroy_widget_window(app),
        _ => {}
    }
}

/// 创建悬浮球窗口：无边框透明置顶、不进任务栏、不可调尺寸、不抢焦点，
/// 先按设置定位再 show（避免闪现在默认位）。全程只 show，绝不 set_focus。
fn create_widget_window(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let settings = storage::load_settings().unwrap_or_default();
    let window = tauri::WebviewWindowBuilder::new(
        app,
        WIDGET_LABEL,
        tauri::WebviewUrl::App("widget.html".into()),
    )
    .title("KimiCodeBar")
    .decorations(false)
    .transparent(true)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    .shadow(false)
    .focused(false)
    .visible(false)
    .build()?;
    apply_widget_layout(&window, &settings);
    let _ = window.show();
    tracing::info!("悬浮球窗口已创建（widget_enabled=true）");
    spawn_fullscreen_guard(app.clone());
    Ok(window)
}

/// 销毁悬浮球窗口（widget_enabled=false）：全屏守卫 ticker 随窗口消失自动退出
fn destroy_widget_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(WIDGET_LABEL) {
        let _ = window.close();
        tracing::info!("悬浮球窗口已销毁（widget_enabled=false）");
    }
}

/// 按设置恢复窗口布局：dock 态贴边细条（先按记忆的 widget_y 落位再吸附），
/// 自由态按 widget_x/y（无效坐标回落默认位）
fn apply_widget_layout(window: &WebviewWindow, settings: &storage::Settings) {
    match settings.widget_dock_edge.as_deref() {
        Some(edge @ ("left" | "right")) => {
            // dock_window 读的是「窗口当前顶边」：冷启动窗口还停在系统给的位置，
            // 不先按记忆 y 落位，贴边细条每次重启都会漂到别处
            if let Some(y) = settings.widget_y {
                if let Some((ax, _, aw, _)) = work_area_of(window) {
                    let x = if edge == "left" {
                        ax
                    } else {
                        ax + aw - DOCK_SIZE.0
                    };
                    let _ = window.set_position(Position::Logical(LogicalPosition::new(x, y)));
                }
            }
            dock_window(window, edge);
        }
        _ => free_window(window, settings),
    }
}

/// 自由态布局：96×96；位置按 widget_x/y 恢复（坐标不在任何显示器工作区内 → 默认位：
/// 主屏工作区右下角内缩 DEFAULT_MARGIN 逻辑像素）
fn free_window(window: &WebviewWindow, settings: &storage::Settings) {
    let app = window.app_handle();
    set_logical_size(window, FREE_SIZE.0, FREE_SIZE.1);
    let pos = settings
        .widget_x
        .zip(settings.widget_y)
        .filter(|(x, y)| position_in_any_work_area(app, *x, *y))
        .unwrap_or_else(|| default_widget_position(app));
    let _ = window.set_position(Position::Logical(LogicalPosition::new(pos.0, pos.1)));
}

/// dock 态布局：12×88 细条贴 left/right 竖边；y 保持窗口当前顶边并 clamp 进工作区
fn dock_window(window: &WebviewWindow, edge: &str) {
    let Some((ax, ay, aw, ah)) = work_area_of(window) else {
        return; // 取不到显示器几何：不动窗口，保持当前位置
    };
    set_logical_size(window, DOCK_SIZE.0, DOCK_SIZE.1);
    let cur_y = window_y_logical(window, ay);
    let y = cur_y.clamp(ay, (ay + ah - DOCK_SIZE.1).max(ay));
    let x = if edge == "left" {
        ax
    } else {
        ax + aw - DOCK_SIZE.0
    };
    let _ = window.set_position(Position::Logical(LogicalPosition::new(x, y)));
}

/// 细条 hover 展开：变回 96×96 且内移对齐贴边（垂直中心与细条保持一致），mouseleave 收回
fn expand_from_dock(window: &WebviewWindow, edge: &str) {
    let Some((ax, ay, aw, ah)) = work_area_of(window) else {
        return;
    };
    set_logical_size(window, FREE_SIZE.0, FREE_SIZE.1);
    let cur_y = window_y_logical(window, ay);
    let y = (cur_y - (FREE_SIZE.1 - DOCK_SIZE.1) / 2.0).clamp(ay, (ay + ah - FREE_SIZE.1).max(ay));
    let x = if edge == "left" {
        ax
    } else {
        ax + aw - FREE_SIZE.0
    };
    let _ = window.set_position(Position::Logical(LogicalPosition::new(x, y)));
}

/// 逻辑尺寸 → set_size（Size::Logical 由 Tauri 按窗口所在显示器缩放比换算，
/// 不手动物理取整：球体无逐帧动画，无亚像素颤动问题）
fn set_logical_size(window: &WebviewWindow, w: f64, h: f64) {
    let _ = window.set_size(Size::Logical(LogicalSize::new(w, h)));
}

/// 窗口所在显示器工作区（逻辑像素）；窗口不在任何显示器上时回落主屏，仍取不到为 None
fn work_area_of(window: &WebviewWindow) -> Option<(f64, f64, f64, f64)> {
    let app = window.app_handle();
    let mon = match window.current_monitor() {
        Ok(Some(m)) => Some(m),
        _ => app.primary_monitor().ok().flatten(),
    }?;
    let scale = mon.scale_factor();
    let wa = mon.work_area();
    Some((
        wa.position.x as f64 / scale,
        wa.position.y as f64 / scale,
        wa.size.width as f64 / scale,
        wa.size.height as f64 / scale,
    ))
}

/// 窗口顶边 y（逻辑像素；读失败回落工作区顶 ay）
fn window_y_logical(window: &WebviewWindow, ay: f64) -> f64 {
    window
        .outer_position()
        .map(|p| p.y as f64 / window.scale_factor().unwrap_or(1.0))
        .unwrap_or(ay)
}

/// 坐标 (x, y) 是否落在任一显示器工作区（逻辑像素）内；显示器枚举失败放行（不卡位置恢复）
fn position_in_any_work_area(app: &AppHandle, x: f64, y: f64) -> bool {
    let monitors = app.available_monitors().unwrap_or_default();
    if monitors.is_empty() {
        return true;
    }
    monitors.iter().any(|m| {
        let scale = m.scale_factor();
        let wa = m.work_area();
        let ax = wa.position.x as f64 / scale;
        let ay = wa.position.y as f64 / scale;
        let aw = wa.size.width as f64 / scale;
        let ah = wa.size.height as f64 / scale;
        x >= ax && x <= ax + aw && y >= ay && y <= ay + ah
    })
}

/// 默认位：主屏工作区右下角内缩 DEFAULT_MARGIN（逻辑像素）；取不到主屏用纯兜底坐标
fn default_widget_position(app: &AppHandle) -> (f64, f64) {
    if let Ok(Some(mon)) = app.primary_monitor() {
        let scale = mon.scale_factor();
        let wa = mon.work_area();
        let ax = wa.position.x as f64 / scale;
        let ay = wa.position.y as f64 / scale;
        let aw = wa.size.width as f64 / scale;
        let ah = wa.size.height as f64 / scale;
        return (
            (ax + aw - FREE_SIZE.0 - DEFAULT_MARGIN).max(ax),
            (ay + ah - FREE_SIZE.1 - DEFAULT_MARGIN).max(ay),
        );
    }
    (DEFAULT_MARGIN, DEFAULT_MARGIN)
}

/// 拖拽结束三字段落盘（widget_x / widget_y / widget_dock_edge）：读改写 settings.json，
/// 只改这三个字段（其余字段原样保留）
fn persist_widget_position(x: f64, y: f64, edge: Option<&str>) {
    let mut settings = storage::load_settings().unwrap_or_default();
    settings.widget_x = Some(x);
    settings.widget_y = Some(y);
    settings.widget_dock_edge = edge.map(str::to_string);
    if let Err(e) = storage::save_settings(&settings) {
        tracing::warn!("悬浮球位置落盘失败: {e}");
    }
}

/// 拖拽静止后前端调用：读窗口位置 → 判贴边 → 两态切换（细条/球）→ 三字段落盘。
/// 返回判定后的 dock_edge（"left"/"right"/None），前端据此切换渲染形态
#[tauri::command]
pub fn widget_drag_ended(app: AppHandle) -> Option<String> {
    let window = app.get_webview_window(WIDGET_LABEL)?;
    let (ax, ay, aw, ah) = work_area_of(&window)?;
    let scale = window.scale_factor().unwrap_or(1.0);
    let pos = window.outer_position().ok()?;
    let size = window.outer_size().ok()?;
    let win_logical = (
        pos.x as f64 / scale,
        pos.y as f64 / scale,
        size.width as f64 / scale,
        size.height as f64 / scale,
    );
    let edge = detect_dock_edge(win_logical, (ax, ay, aw, ah));
    match edge {
        Some(e) => {
            // 两态切换：吸附到边缘收缩成细条；落盘坐标 = 吸附后的细条位置
            dock_window(&window, e);
            let x = if e == "left" {
                ax
            } else {
                ax + aw - DOCK_SIZE.0
            };
            let y = win_logical.1.clamp(ay, (ay + ah - DOCK_SIZE.1).max(ay));
            persist_widget_position(x, y, Some(e));
        }
        None => {
            set_logical_size(&window, FREE_SIZE.0, FREE_SIZE.1);
            // 自由态落位钳进工作区：拖拽中窗口可能被推出屏幕边缘（抓着球往边上顶），
            // 直接落盘会把「半挂屏外」记成位置——下次冷启动判「不在工作区」跳回默认位
            let x = win_logical.0.clamp(ax, (ax + aw - FREE_SIZE.0).max(ax));
            let y = win_logical.1.clamp(ay, (ay + ah - FREE_SIZE.1).max(ay));
            let _ = window.set_position(Position::Logical(LogicalPosition::new(x, y)));
            persist_widget_position(x, y, None);
        }
    }
    edge.map(str::to_string)
}

/// 细条 hover 展开/收回：expanded=true 变回 96×96 并内移对齐贴边（垂直中心对齐细条），
/// false 收回细条。自由态（未贴边）为空操作。
/// 返回「窗口最终是否处于收起（细条）形态」——前端据此同步自己的形态状态：
/// 收回前先复查真实光标位置，指针还在窗内（窗口缩放瞬间 WebView2 合成的 mouseleave）
/// 则忽略本次收回，否则「hover 展开即收回」会闪一下
#[tauri::command]
pub fn widget_set_expanded(app: AppHandle, expanded: bool) -> bool {
    let Some(window) = app.get_webview_window(WIDGET_LABEL) else {
        return false;
    };
    let edge = storage::load_settings()
        .ok()
        .and_then(|s| s.widget_dock_edge)
        .unwrap_or_default();
    match edge.as_str() {
        "left" | "right" => {
            if expanded {
                expand_from_dock(&window, &edge);
                false
            } else if cursor_inside_window(&window) {
                false // 指针没动：合成的 mouseleave，保持展开
            } else {
                dock_window(&window, &edge);
                true
            }
        }
        _ => false, // 自由态：窗口本就是 96×96，无展开语义
    }
}

/// 点击球体打开主面板：复用托盘左键显示面板路径（优先托盘定位，取不到 rect 退化 show+focus）
#[tauri::command]
pub fn open_main_panel(app: AppHandle) {
    tracing::info!("悬浮球点击：打开主面板");
    crate::panel::show_panel_for_second_instance(&app);
}

/// 球前端首屏布局：当前贴边状态（"left"/"right"/None），前端据此决定渲染球还是细条。
/// （WidgetState 契约只含展示数据不含布局，布局由本命令 + widget_drag_ended 返回值维护）
#[tauri::command]
pub fn get_widget_layout() -> Option<String> {
    storage::load_settings()
        .unwrap_or_default()
        .widget_dock_edge
}

/// 球前端首屏数据：当前内存态快照 + 设置口径解析（浏览器/球窗启动时立即调用一次）
#[tauri::command]
pub fn get_widget_state(app: AppHandle) -> WidgetState {
    let panel = app.state::<AppState>().snapshot();
    let settings = storage::load_settings().unwrap_or_default();
    compute_widget_state(&panel, &settings)
}

/// 全屏守卫 ticker：球窗口存在期间每 FULLSCREEN_GUARD_SECS 探测一次，全屏命中 hide、
/// 退出 show；命中/恢复各落一行日志（仿轮询探针行，供「球还在不在」类问题对照）。
/// 窗口销毁或被新一轮守卫接替（代次不符）时循环自退出
fn spawn_fullscreen_guard(app: AppHandle) {
    let gen = WIDGET_GUARD_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    tauri::async_runtime::spawn(async move {
        let mut ticker =
            tokio::time::interval(std::time::Duration::from_secs(FULLSCREEN_GUARD_SECS));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut hidden_by_guard = false;
        loop {
            ticker.tick().await;
            if WIDGET_GUARD_SEQ.load(Ordering::SeqCst) != gen {
                return; // 窗口重建过：守卫已由新一轮接管
            }
            let Some(window) = app.get_webview_window(WIDGET_LABEL) else {
                tracing::debug!("悬浮球窗口已销毁，全屏守卫 ticker 退出");
                return;
            };
            match (
                kimicodebar::fullscreen::fullscreen_app_active(),
                hidden_by_guard,
            ) {
                (true, false) => {
                    let _ = window.hide();
                    hidden_by_guard = true;
                    tracing::info!("全屏守卫命中：悬浮球已隐藏（退出全屏后自动恢复）");
                }
                (false, true) => {
                    let _ = window.show();
                    hidden_by_guard = false;
                    tracing::info!("全屏守卫解除：悬浮球已恢复显示");
                }
                _ => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 快照构造 helper：五小时/七天剩余百分比（配额类账号）
    fn snap(id: &str, name: &str, five: Option<f64>, weekly: Option<f64>) -> WidgetAccountSnapshot {
        WidgetAccountSnapshot {
            id: id.to_string(),
            name: name.to_string(),
            five_hour_pct: five,
            weekly_pct: weekly,
            balance: None,
            balance_currency: None,
            balance_pct: None,
        }
    }

    /// 快照构造 helper：余额类账号（DeepSeek）
    fn snap_balance(id: &str, name: &str, balance: f64, pct: f64) -> WidgetAccountSnapshot {
        WidgetAccountSnapshot {
            id: id.to_string(),
            name: name.to_string(),
            five_hour_pct: None,
            weekly_pct: None,
            balance: Some(balance),
            balance_currency: Some("CNY".to_string()),
            balance_pct: Some(pct),
        }
    }

    // ---- select_widget_account（任务书 1a）----

    #[test]
    fn select_specified_account_with_quota_wins() {
        let s = vec![
            snap("a", "账号 A", Some(80.0), Some(70.0)),
            snap("b", "账号 B", Some(50.0), Some(10.0)),
        ];
        // 指定 id 存在且 quota 非空 → 它（即使不是最紧张的）
        let picked = select_widget_account(&s, Some("a")).unwrap();
        assert_eq!(picked.id, "a");
        assert_eq!(picked.name, "账号 A");
    }

    #[test]
    fn select_specified_falls_back_to_auto_when_missing_or_empty() {
        let s = vec![snap("a", "账号 A", Some(80.0), Some(70.0))];
        // 指定的 id 不存在 → 回落自动（最紧张者 = 唯一有数据的 a）
        assert_eq!(select_widget_account(&s, Some("nope")).unwrap().id, "a");
        // 指定的账号无配额数据（quota=None）→ 同样回落自动
        let s2 = vec![
            snap("a", "账号 A", Some(80.0), Some(70.0)),
            snap("b", "空账号", None, None),
        ];
        assert_eq!(select_widget_account(&s2, Some("b")).unwrap().id, "a");
    }

    #[test]
    fn select_auto_picks_tightest_account() {
        let s = vec![
            snap("a", "A", Some(80.0), Some(70.0)), // min = 70
            snap("b", "B", Some(50.0), Some(10.0)), // min = 10 ← 最紧张
            snap("c", "C", Some(60.0), None),       // min = 60
        ];
        assert_eq!(select_widget_account(&s, None).unwrap().id, "b");
        // "auto" 显式值与 None 同义（设置层 None/"auto" 都不进指定分支）
        assert_eq!(select_widget_account(&s, Some("auto")).unwrap().id, "b");
    }

    #[test]
    fn select_tie_prefers_earlier_in_list() {
        let s = vec![
            snap("a", "A", Some(30.0), None),
            snap("b", "B", None, Some(30.0)),
        ];
        // 并列最低取列表顺序靠前者
        assert_eq!(select_widget_account(&s, None).unwrap().id, "a");
    }

    #[test]
    fn select_excludes_deepseek_and_all_empty_returns_none() {
        // 无任何数据的账号（quota 与余额都空）不参与自动选择
        let s = vec![
            snap("ds", "DeepSeek", None, None),
            snap("a", "A", Some(40.0), Some(55.0)),
        ];
        assert_eq!(select_widget_account(&s, None).unwrap().id, "a");
        // 全部无数据 → None
        let empty = vec![snap("ds", "DeepSeek", None, None)];
        assert!(select_widget_account(&empty, None).is_none());
        // 指定该账号且无其他账号 → None
        assert!(select_widget_account(&empty, Some("ds")).is_none());
        // 空列表 → None
        assert!(select_widget_account(&[], None).is_none());
    }

    // ---- 余额类账号（DeepSeek 余额上球，2026-10-02 用户需求）----

    #[test]
    fn select_specified_balance_account_wins() {
        let s = vec![
            snap("a", "Kimi", Some(80.0), Some(70.0)),
            snap_balance("ds", "DeepSeek", 12.34, 246.8),
        ];
        // 指定余额账号 → 它（即使配额账号存在且「更紧张」）
        let picked = select_widget_account(&s, Some("ds")).unwrap();
        assert_eq!(picked.id, "ds");
        assert_eq!(picked.balance, Some(12.34));
    }

    #[test]
    fn select_auto_prefers_quota_over_balance() {
        // 自动档：有配额账号在，就不混入余额账号（不同量纲不混算），
        // 哪怕余额健康度更低（40 vs 70）
        let s = vec![
            snap("a", "Kimi", Some(80.0), Some(70.0)),
            snap_balance("ds", "DeepSeek", 2.0, 40.0),
        ];
        assert_eq!(select_widget_account(&s, None).unwrap().id, "a");
    }

    #[test]
    fn select_auto_falls_back_to_lowest_balance() {
        // 一个配额账号都没有（只有余额账号）→ 取健康度最低的余额账号
        let s = vec![
            snap_balance("ds1", "DS 富余", 50.0, 1000.0),
            snap_balance("ds2", "DS 紧张", 3.0, 60.0),
            snap("empty", "空账号", None, None),
        ];
        assert_eq!(select_widget_account(&s, None).unwrap().id, "ds2");
    }

    #[test]
    fn balance_pct_math() {
        // 余额 / 告警线 × 100：100 = 正好在告警线
        assert_eq!(balance_pct(Some(12.5), 5.0), Some(250.0));
        assert_eq!(balance_pct(Some(5.0), 5.0), Some(100.0));
        assert_eq!(balance_pct(Some(2.5), 5.0), Some(50.0));
        // 告警线未配置（≤0）按充裕（200）
        assert_eq!(balance_pct(Some(2.5), 0.0), Some(200.0));
        // 取不到余额 → None
        assert_eq!(balance_pct(None, 5.0), None);
        // 负数异常值钳到 0
        assert_eq!(balance_pct(Some(-1.0), 5.0), Some(0.0));
    }

    // ---- widget_center_pct（任务书 1b）----

    #[test]
    fn center_metric_pinned_value_when_available() {
        assert_eq!(
            widget_center_pct(Some(36.0), Some(87.0), Some("five_hour")),
            Some(36.0)
        );
        assert_eq!(
            widget_center_pct(Some(36.0), Some(87.0), Some("weekly")),
            Some(87.0)
        );
    }

    #[test]
    fn center_metric_falls_back_to_auto_when_pinned_missing() {
        // 指定口径该项无数据 → 回落 auto（已有项里最低）
        assert_eq!(
            widget_center_pct(None, Some(87.0), Some("five_hour")),
            Some(87.0)
        );
        assert_eq!(
            widget_center_pct(Some(36.0), None, Some("weekly")),
            Some(36.0)
        );
    }

    #[test]
    fn center_auto_takes_lowest_available() {
        assert_eq!(widget_center_pct(Some(36.0), Some(87.0), None), Some(36.0));
        assert_eq!(
            widget_center_pct(Some(87.0), Some(36.0), Some("auto")),
            Some(36.0)
        );
        // 未知 metric 值按 auto
        assert_eq!(
            widget_center_pct(Some(87.0), Some(36.0), Some("bogus")),
            Some(36.0)
        );
        // 只有一项有数据 → 该项
        assert_eq!(widget_center_pct(None, Some(42.0), None), Some(42.0));
        assert_eq!(widget_center_pct(Some(42.0), None, None), Some(42.0));
        // 两项都无 → None
        assert_eq!(widget_center_pct(None, None, None), None);
        assert_eq!(widget_center_pct(None, None, Some("five_hour")), None);
    }

    // ---- detect_dock_edge（任务书 1c）----

    #[test]
    fn dock_edge_left_and_right_within_threshold() {
        // 1080p 工作区，窗口 96 宽：距左缘 12 内 → left；距右缘 12 内 → right
        assert_eq!(
            detect_dock_edge((0.0, 100.0, 96.0, 96.0), (0.0, 0.0, 1920.0, 1040.0)),
            Some("left")
        );
        assert_eq!(
            detect_dock_edge((12.0, 100.0, 96.0, 96.0), (0.0, 0.0, 1920.0, 1040.0)),
            Some("left")
        );
        assert_eq!(
            detect_dock_edge((1812.0, 100.0, 96.0, 96.0), (0.0, 0.0, 1920.0, 1040.0)),
            Some("right")
        );
        assert_eq!(
            detect_dock_edge((1824.0, 100.0, 96.0, 96.0), (0.0, 0.0, 1920.0, 1040.0)),
            Some("right")
        );
    }

    #[test]
    fn dock_edge_boundary_at_threshold() {
        // 恰好 12（含端点算贴边）与 12.1（超出不算）
        assert_eq!(
            detect_dock_edge((12.0, 0.0, 96.0, 96.0), (0.0, 0.0, 1920.0, 1040.0)),
            Some("left")
        );
        assert_eq!(
            detect_dock_edge((12.1, 0.0, 96.0, 96.0), (0.0, 0.0, 1920.0, 1040.0)),
            None
        );
        // 右缘：右距 12 恰好命中 / 12.1 不命中
        let right_at = 1920.0 - 96.0 - 12.0;
        assert_eq!(
            detect_dock_edge((right_at, 0.0, 96.0, 96.0), (0.0, 0.0, 1920.0, 1040.0)),
            Some("right")
        );
        assert_eq!(
            detect_dock_edge(
                (right_at - 0.1, 0.0, 96.0, 96.0),
                (0.0, 0.0, 1920.0, 1040.0)
            ),
            None
        );
    }

    #[test]
    fn dock_edge_slight_overshoot_still_docks() {
        // 拖拽略越出边缘（Windows 允许部分出屏）：负距离在容差内仍判贴边
        assert_eq!(
            detect_dock_edge((-6.0, 0.0, 96.0, 96.0), (0.0, 0.0, 1920.0, 1040.0)),
            Some("left")
        );
        // 右缘越出 6px：窗口右缘 1926 = x(1830) + 宽(96)
        assert_eq!(
            detect_dock_edge((1830.0, 0.0, 96.0, 96.0), (0.0, 0.0, 1920.0, 1040.0)),
            Some("right")
        );
        // 越出超过阈值也算贴边（2026-10-02 修）：用户按住球往边上顶，窗口随抓点越出屏幕
        // 最多一个球宽，只认 ±12 会让「推到最边」永远判不到（实机复现越出 46px）
        // 左缘越出 46px（抓球心推到极左的实测值）
        assert_eq!(
            detect_dock_edge((-46.0, 0.0, 96.0, 96.0), (0.0, 0.0, 1920.0, 1040.0)),
            Some("left")
        );
        // 右缘越出 46px：窗口右缘 1966 = x(1870) + 宽(96)
        assert_eq!(
            detect_dock_edge((1870.0, 0.0, 96.0, 96.0), (0.0, 0.0, 1920.0, 1040.0)),
            Some("right")
        );
        // 整个窗口都被推出屏幕（越出 600px）仍算贴边：吸附回来反而把球拉回可见区
        assert_eq!(
            detect_dock_edge((-600.0, 0.0, 96.0, 96.0), (0.0, 0.0, 1920.0, 1040.0)),
            Some("left")
        );
        // 但停在屏中间不算（离边 400px 的自由位）
        assert_eq!(
            detect_dock_edge((400.0, 0.0, 96.0, 96.0), (0.0, 0.0, 1920.0, 1040.0)),
            None
        );
    }

    #[test]
    fn dock_edge_free_position_returns_none() {
        assert_eq!(
            detect_dock_edge((400.0, 300.0, 96.0, 96.0), (0.0, 0.0, 1920.0, 1040.0)),
            None
        );
        // 带负坐标的多显示器工作区（副屏在左）
        assert_eq!(
            detect_dock_edge((-1910.0, 0.0, 96.0, 96.0), (-1920.0, 0.0, 1920.0, 1040.0)),
            Some("left")
        );
    }

    #[test]
    fn dock_edge_both_sides_takes_closer() {
        // 屏极窄（宽 < 窗口 + 两侧阈值）：两边都命中，取距离更近的一边
        assert_eq!(
            detect_dock_edge((2.0, 0.0, 96.0, 96.0), (0.0, 0.0, 100.0, 1040.0)),
            Some("left")
        );
        assert_eq!(
            detect_dock_edge((3.0, 0.0, 96.0, 96.0), (0.0, 0.0, 100.0, 1040.0)),
            Some("right")
        );
    }

    // ---- point_in_rect（收细条前的光标复查，任务 3 交互修复）----

    #[test]
    fn point_in_rect_boundaries() {
        let r = (100, 50, 200, 150);
        // 内部与左上角（含）命中
        assert!(point_in_rect((150, 100), r));
        assert!(point_in_rect((100, 50), r));
        // 右/下边界取开区间（恰好贴边不算「还在窗内」，指针已到窗外）
        assert!(!point_in_rect((200, 100), r));
        assert!(!point_in_rect((150, 150), r));
        // 窗外
        assert!(!point_in_rect((99, 100), r));
        assert!(!point_in_rect((150, 49), r));
        // 负坐标（副屏在左）工作区
        assert!(point_in_rect((-50, 0), (-100, -50, 0, 50)));
    }

    // ---- compute_widget_state（组装口径）----

    #[test]
    fn compute_state_picks_account_and_center() {
        let panel = crate::commands::PanelState {
            loading: false,
            accounts: vec![
                crate::commands::AccountPanel {
                    account: kimicodebar::storage::Account {
                        id: "a".into(),
                        name: "账号 A".into(),
                        ..Default::default()
                    },
                    credential: true,
                    quota: Some(kimicodebar::quota::KimiQuota {
                        five_hour: Some(kimicodebar::quota::QuotaDetail {
                            percent_remaining: 36.0,
                            ..Default::default()
                        }),
                        weekly: Some(kimicodebar::quota::QuotaDetail {
                            percent_remaining: 87.0,
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                crate::commands::AccountPanel {
                    account: kimicodebar::storage::Account {
                        id: "ds".into(),
                        name: "DeepSeek".into(),
                        ..Default::default()
                    },
                    credential: true,
                    quota: None,
                    ..Default::default()
                },
            ],
        };
        let mut settings = storage::Settings {
            widget_center_metric: Some("weekly".into()),
            ..Default::default()
        };
        let state = compute_widget_state(&panel, &settings);
        assert!(state.has_data);
        assert_eq!(state.account_name, "账号 A");
        assert_eq!(state.five_hour_pct, Some(36.0));
        assert_eq!(state.weekly_pct, Some(87.0));
        assert_eq!(state.center_pct, Some(87.0));

        // 口径 auto：取最紧张项
        settings.widget_center_metric = None;
        assert_eq!(
            compute_widget_state(&panel, &settings).center_pct,
            Some(36.0)
        );

        // 指定 DeepSeek（quota=None）→ 回落自动 → 仍是账号 A
        settings.widget_account_id = Some("ds".into());
        let state = compute_widget_state(&panel, &settings);
        assert!(state.has_data);
        assert_eq!(state.account_name, "账号 A");

        // 全部账号无数据 → 占位态
        let empty_panel = crate::commands::PanelState {
            loading: false,
            accounts: vec![],
        };
        settings.widget_account_id = None;
        let state = compute_widget_state(&empty_panel, &settings);
        assert!(!state.has_data);
        assert_eq!(state.account_name, "");
        assert_eq!(state.center_pct, None);
    }

    #[test]
    fn compute_state_balance_account_carries_balance() {
        // 余额类账号（DeepSeek）上球：配额三字段为空，balance* 三字段有值
        let panel = crate::commands::PanelState {
            loading: false,
            accounts: vec![
                crate::commands::AccountPanel {
                    account: kimicodebar::storage::Account {
                        id: "ds".into(),
                        name: "DeepSeek".into(),
                        ..Default::default()
                    },
                    credential: true,
                    quota: None,
                    deepseek_balance: Some(kimicodebar::deepseek::models::DeepSeekBalance {
                        is_available: true,
                        currency: "CNY".into(),
                        total_balance: 12.34,
                        granted_balance: 0.0,
                        topped_up_balance: 12.34,
                    }),
                    ..Default::default()
                },
                crate::commands::AccountPanel {
                    account: kimicodebar::storage::Account {
                        id: "a".into(),
                        name: "账号 A".into(),
                        ..Default::default()
                    },
                    credential: true,
                    quota: Some(kimicodebar::quota::KimiQuota {
                        five_hour: Some(kimicodebar::quota::QuotaDetail {
                            percent_remaining: 36.0,
                            ..Default::default()
                        }),
                        weekly: Some(kimicodebar::quota::QuotaDetail {
                            percent_remaining: 87.0,
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ],
        };
        let mut settings = storage::Settings {
            widget_account_id: Some("ds".into()),
            deepseek_warn_threshold: 5.0,
            ..Default::default()
        };
        let state = compute_widget_state(&panel, &settings);
        assert!(state.has_data);
        assert_eq!(state.account_name, "DeepSeek");
        assert_eq!(state.five_hour_pct, None);
        assert_eq!(state.weekly_pct, None);
        assert_eq!(state.center_pct, None);
        assert_eq!(state.balance, Some(12.34));
        assert_eq!(state.balance_currency.as_deref(), Some("CNY"));
        assert_eq!(state.balance_pct, Some(246.8)); // 12.34 / 5 × 100

        // 换成配额账号：balance* 三字段清空（两种口径不混）
        settings.widget_account_id = Some("a".into());
        let state = compute_widget_state(&panel, &settings);
        assert_eq!(state.balance, None);
        assert_eq!(state.balance_currency, None);
        assert_eq!(state.balance_pct, None);
        assert_eq!(state.five_hour_pct, Some(36.0));
    }
}
