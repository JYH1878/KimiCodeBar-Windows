//! 桌面悬浮球（issue #58）：常驻桌面的 5 小时/7 天额度双环球。
//!
//! 形态与交互（任务书拍板）：
//! - 双环球 = 外环 7 天（weekly）、内环 5 小时（five_hour），中心数字为当前口径剩余百分比；
//! - 可拖拽（系统 startDragging），拖拽静止 300ms 后前端调 `widget_drag_ended`，
//!   后端按窗口与所在显示器工作区的关系判定贴边：贴左右竖边自动收缩成 12×88 细条
//!   （hover 展开回 96×96），自由悬浮保持 96×96；位置与贴边三字段落盘；
//! - 全屏应用活跃时自动隐藏，退出恢复；判定独立于托盘/通知的共享守卫：
//!   几何 ∪ QUNS∈{3,4}，另补「QUNS=2 + 铺满窗口扫描佐证」（裸 QUNS=2 不藏——
//!   2026-10-03 QUNS 卡 2 事故；佐证分支防启动/重建期前台被本进程隐藏窗口占据
//!   导致的几何失明，见 fullscreen::widget_hide_decision_with_covering），
//!   「全屏应用时自动隐藏」可在设置关闭；hide/show 失败保留状态下一轮重试，
//!   守卫初态与创建期结果一致（防藏死），开关关闭后最迟一周期恢复；
//! - 窗口懒创建懒销毁：`widget_enabled` 开才建（启动 / save_settings 后 reconcile），
//!   关即销毁，保低内存卖点。销毁生命周期（2026-10-29 坑 57）：close() 后 Tauri
//!   窗口表条目可长期残留（WebView2 异步拆卸，本机实测 ≥15 秒），期间 reconcile
//!   误判「窗口健在」跳过重建 → 快速关开后球永久消失。正解 = hide() 先行 +
//!   destroy() 强制销毁 + `WindowEvent::Destroyed` 确认（已请求 ≠ 已销毁）+
//!   确认后按最新设置自动补建；守卫 ticker 兼任窗口表监视器兜底。
//!
//! 坐标铁律（panel.rs 教训）：一律逻辑像素，所在显示器 scale_factor 换算只在读
//! 系统值（work_area / outer_position）出物理坐标时使用，禁止把物理回读值直接喂回
//! set_size / set_position（单位语义不一致会把窗口越撑越大）。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

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
/// 窗口实例代次：每创建一个窗口 +1；Destroyed 回调携带创建期代次，与当前代次
/// 不符即旧窗口的迟到/重复通知，忽略（防旧代次回调误建新窗）
static WIDGET_INSTANCE_SEQ: AtomicU64 = AtomicU64::new(0);
/// 销毁确认超时（秒）：超过后仍见窗口表条目 = Tauri 侧拆卸卡住（坑 57 僵尸），
/// 落 warn 如实告知；恢复不靠重试销毁，靠守卫监视窗口表 + 每次保存的 reconcile
const DESTROY_CONFIRM_TIMEOUT_SECS: u64 = 5;

/// 悬浮球窗口生命周期簿记（快速开关核心状态；纯决策函数只吃快照，单测不碰全局）
#[derive(Debug, Default)]
struct WidgetLifecycle {
    /// 当前窗口代次（0 = 从未创建过窗口）
    generation: u64,
    /// destroy() 已请求、尚未收到 Destroyed 确认——此时 Tauri 窗口表里是
    /// 「僵尸条目」：get_webview_window 仍命中，但窗口已不可交互/拆卸中
    destroy_pending: bool,
    /// 销毁请求时刻（超时告警用；destroy_pending=false 时为 None）
    destroy_requested_at: Option<Instant>,
    /// 本代是否已发过销毁确认超时告警（只警一次，防守卫每 2 秒刷屏）
    destroy_timeout_warned: bool,
}

impl WidgetLifecycle {
    const fn new() -> Self {
        WidgetLifecycle {
            generation: 0,
            destroy_pending: false,
            destroy_requested_at: None,
            destroy_timeout_warned: false,
        }
    }
}

/// 生命周期全局状态（锁中毒不致命：恢复数据继续用）
static WIDGET_LIFECYCLE: Mutex<WidgetLifecycle> = Mutex::new(WidgetLifecycle::new());
/// reconcile 串行锁：Destroyed 回调 / 守卫 ticker / save_settings 三处都可能触发
/// 补建，并发下「都判无窗口→都建」会撞 label（第二次报已存在错）——串行后天然去重
static WIDGET_RECONCILE_LOCK: Mutex<()> = Mutex::new(());

fn lifecycle_lock() -> MutexGuard<'static, WidgetLifecycle> {
    WIDGET_LIFECYCLE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

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

/// reconcile 的纯决策（坑 57 核心）：输入（期望开关、Tauri 窗口表是否有本 label、
/// 是否销毁中）→ 动作。窗口表命中但销毁未确认 = 僵尸条目，不能当「健在」用：
/// 应开时等 Destroyed 确认后补建（确认回调会再调 reconcile），绝不在僵尸上无操作
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WidgetReconcileAction {
    /// 应开而无窗口（销毁已确认或从未建过）→ 创建
    Create,
    /// 应关而窗口健在 → 请求销毁
    Destroy,
    /// 应开但旧窗口销毁未确认 → 等 Destroyed 确认后自动补建
    AwaitDestroyConfirm,
    /// 无事可做（开+健在 / 关+无窗 / 关+销毁中已请求过）
    None,
}

pub(crate) fn widget_reconcile_decision(
    enabled: bool,
    window_in_table: bool,
    destroy_pending: bool,
) -> WidgetReconcileAction {
    match (enabled, window_in_table, destroy_pending) {
        (true, false, _) => WidgetReconcileAction::Create,
        (true, true, false) => WidgetReconcileAction::None,
        (true, true, true) => WidgetReconcileAction::AwaitDestroyConfirm,
        (false, true, false) => WidgetReconcileAction::Destroy,
        (false, true, true) => WidgetReconcileAction::None,
        (false, false, _) => WidgetReconcileAction::None,
    }
}

/// Destroyed 确认的纯决策：通知携带的代次 == 当前代次（且 >0）→ 这条确认属于
/// 当前窗口，有效（应触发补建 reconcile）；否则是旧代次的迟到/重复通知，忽略
/// （防「重复完成通知」或旧窗口回调误建新窗）
pub(crate) fn widget_destroyed_notification(
    current_generation: u64,
    notify_generation: u64,
) -> bool {
    current_generation != 0 && current_generation == notify_generation
}

/// 在阻塞线程上执行 reconcile：Tauri 同步命令跑在主线程，窗口 build 嵌套在 IPC
/// 派发内执行会死锁（2026-10-29 实测：save_settings 内直接调用 → build 永不返回，
/// 保存也挂死）；三个触发源（保存 / Destroyed 确认 / 守卫兜底）统一走这里
pub(crate) fn spawn_reconcile(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || reconcile(&app));
}

/// 按设置收敛悬浮球窗口的存在性（启动 / save_settings / Destroyed 确认后调用）：
/// 开 → 懒创建；关 → 销毁；销毁中重开 → 等确认自动补建。
/// 串行锁防三处触发源并发补建撞 label（见 WIDGET_RECONCILE_LOCK）。
/// ⚠️ 别在 Tauri 同步命令上下文直接调用（主线程嵌套建窗死锁），用 spawn_reconcile
pub fn reconcile(app: &AppHandle) {
    let _serial = WIDGET_RECONCILE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let enabled = storage::load_settings()
        .map(|s| s.widget_enabled)
        .unwrap_or(false);
    let window_in_table = app.get_webview_window(WIDGET_LABEL).is_some();
    let (generation, destroy_pending, destroy_age, timeout_warned) = {
        let lc = lifecycle_lock();
        (
            lc.generation,
            lc.destroy_pending,
            lc.destroy_requested_at.map(|t| t.elapsed()),
            lc.destroy_timeout_warned,
        )
    };
    match widget_reconcile_decision(enabled, window_in_table, destroy_pending) {
        WidgetReconcileAction::Create => {
            tracing::info!(
                "悬浮球 reconcile → 创建（enabled={enabled} 窗口在表={window_in_table} 销毁中={destroy_pending}）"
            );
            if let Err(e) = create_widget_window(app) {
                tracing::warn!("创建悬浮球窗口失败（下次保存/触发时重试）: {e}");
            }
        }
        WidgetReconcileAction::Destroy => {
            tracing::info!(
                "悬浮球 reconcile → 销毁（enabled={enabled} 窗口在表={window_in_table}）"
            );
            destroy_widget_window(app);
        }
        WidgetReconcileAction::AwaitDestroyConfirm => {
            // 销毁确认由 Destroyed 回调驱动（补建在那边）；此处只负责如实记账：
            // 常规等待打 info，超时（Tauri 窗口表残留僵尸）打一次 warn——恢复路径
            // 不变：守卫监视窗口表移除 + 每次保存都会再进这里
            let timeout = std::time::Duration::from_secs(DESTROY_CONFIRM_TIMEOUT_SECS);
            if destroy_age.is_some_and(|age| age >= timeout) {
                if !timeout_warned {
                    tracing::warn!(
                        "悬浮球销毁确认超时（第 {generation} 代，已 {:?}，Tauri 窗口表仍残留旧条目）\
                         ——守卫将持续监视，条目移除后按最新设置自动补建",
                        destroy_age.unwrap_or_default()
                    );
                    lifecycle_lock().destroy_timeout_warned = true;
                }
            } else {
                tracing::info!(
                    "旧悬浮球窗口销毁中（第 {generation} 代），销毁确认后按最新设置自动补建"
                );
            }
        }
        WidgetReconcileAction::None => {}
    }
}

/// 创建悬浮球窗口：无边框透明置顶、不进任务栏、不可调尺寸、不抢焦点，
/// 先按设置定位再 show（避免闪现在默认位）。全程只 show，绝不 set_focus。
/// 创建即登记代次并挂 Destroyed 监听（销毁确认 + 补建的唯二入口之一）
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
    let generation = WIDGET_INSTANCE_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut lc = lifecycle_lock();
        lc.generation = generation;
        lc.destroy_pending = false;
        lc.destroy_requested_at = None;
        lc.destroy_timeout_warned = false;
    }
    // Destroyed 确认：窗口真正从 Tauri 窗口表移除后回调（tauri 先移表项再派发监听器）。
    // 代次不符 = 旧窗口的迟到/重复通知 → 忽略；有效确认 → 清销毁中状态并按最新
    // 设置补建。回调跑在事件循环线程，建窗须转异步任务（同线程投递建窗消息会卡死）
    let listen_app = app.clone();
    window.on_window_event(move |event| {
        if !matches!(event, tauri::WindowEvent::Destroyed) {
            return;
        }
        let current = lifecycle_lock().generation;
        if !widget_destroyed_notification(current, generation) {
            tracing::debug!(
                "悬浮球旧代次销毁通知（通知第 {generation} 代，当前第 {current} 代），忽略"
            );
            return;
        }
        {
            let mut lc = lifecycle_lock();
            lc.destroy_pending = false;
            lc.destroy_requested_at = None;
        }
        tracing::info!("悬浮球窗口已确认销毁（第 {generation} 代），按最新设置补建检查");
        spawn_reconcile(&listen_app);
    });
    apply_widget_layout(&window, &settings);
    // 创建期全屏判定（与守卫每跳同款 widget_hide_decision_with_covering）：稳态矩阵
    // 之外补「QUNS=2 + 存在铺满屏幕的可见全屏窗口（covering 扫描，不看前台）」——
    // 启动/重建时本进程的隐藏窗口可能被 WebView2 激活成前台（own-pid 排除 → 前台
    // 几何失明），此时 QUNS=2 有铺满窗口佐证 → 先藏，防全屏中闪现；守卫以
    // initially_hidden 接力（状态与窗口实际可见性一致，退出全屏后有人发 Show）。
    // 传感器随创建日志落盘（验收取证：创建瞬间前台归属 / QUNS / 几何 / 铺满扫描）
    let probe = kimicodebar::fullscreen::probe();
    let covering = kimicodebar::fullscreen::covering_fullscreen_window_present();
    let hide = kimicodebar::fullscreen::widget_hide_decision_with_covering(
        probe.quns,
        probe.heuristic,
        covering,
        settings.widget_auto_hide,
    );
    let ctx = guard_log_ctx(settings.widget_auto_hide, &probe, covering);
    if hide {
        tracing::info!("悬浮球创建于全屏期：保持隐藏不闪显（{ctx}）");
    } else {
        // 显式 show 并核查结果（失败记 warn，守卫下一轮如需恢复会再试）
        if let Err(e) = window.show() {
            tracing::warn!("悬浮球窗口显示失败: {e}");
        }
        tracing::info!("悬浮球窗口已创建并显示（{ctx}）");
    }
    spawn_fullscreen_guard(app.clone(), hide);
    Ok(window)
}

/// 销毁悬浮球窗口（widget_enabled=false）：先立即 hide（WebView2 异步拆卸可能拖
/// 数秒，置顶旧窗残留桌面会拖累面板/设置窗的焦点与点击），再 destroy() 强制销毁
/// （跳过 CloseRequested）。destroy() 返回 Ok 只代表请求已受理——真正完成以
/// Destroyed 确认为准（已请求 ≠ 已销毁，坑 57：旧代码 close() 后立刻报「已销毁」
/// 是提前报喜，且窗口表僵尸条目让重开永远跳过创建）
fn destroy_widget_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window(WIDGET_LABEL) else {
        return;
    };
    if let Err(e) = window.hide() {
        tracing::warn!("悬浮球窗口隐藏失败（销毁前置）: {e}");
    }
    match window.destroy() {
        Ok(()) => {
            let generation = {
                let mut lc = lifecycle_lock();
                lc.destroy_pending = true;
                lc.destroy_requested_at = Some(Instant::now());
                lc.generation
            };
            tracing::info!(
                "悬浮球窗口销毁已请求（第 {generation} 代，widget_enabled=false），等待销毁确认"
            );
        }
        Err(e) => {
            // 失败保留「健在」记账（destroy_pending 不置位）：下次 reconcile 重走
            // Destroy 分支重试；窗口已被 hide 至少不再干扰交互
            tracing::warn!("悬浮球窗口销毁请求失败: {e}");
        }
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

/// 全屏守卫单步动作（纯判定）：由「本轮应否隐藏 × 当前是否被守卫隐藏」推出。
/// 恢复分支同时覆盖「退出全屏」与「widget_auto_hide 关闭」（两者都令 should_hide=false），
/// 因此开关关闭时被藏的球最迟下一周期（≤FULLSCREEN_GUARD_SECS）恢复
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WidgetGuardAction {
    /// 全屏命中且当前可见 → hide
    Hide,
    /// 不再命中且当前被守卫隐藏 → show
    Show,
    /// 状态无变化 → 不动窗口（也就不刷日志，防每 2 秒刷屏）
    None,
}

pub(crate) fn widget_guard_action(should_hide: bool, hidden_by_guard: bool) -> WidgetGuardAction {
    match (should_hide, hidden_by_guard) {
        (true, false) => WidgetGuardAction::Hide,
        (false, true) => WidgetGuardAction::Show,
        _ => WidgetGuardAction::None,
    }
}

/// 执行守卫动作：hide/show 的 Result 必须核查——只有成功才更新 hidden_by_guard；
/// 失败记 warn（带传感器上下文）保留原状态，下一轮 tick 按原状态重算出同一动作重试。
/// 返回执行后的隐藏状态。只在动作实际发生时落一行日志（info=成功转换 / warn=失败重试）
fn apply_guard_action<E: std::fmt::Display>(
    action: WidgetGuardAction,
    hidden_by_guard: bool,
    ctx: &str,
    hide: impl FnOnce() -> Result<(), E>,
    show: impl FnOnce() -> Result<(), E>,
) -> bool {
    match action {
        WidgetGuardAction::Hide => match hide() {
            Ok(()) => {
                tracing::info!("全屏守卫隐藏悬浮球（{ctx}）");
                true
            }
            Err(e) => {
                tracing::warn!("悬浮球隐藏失败，下一轮重试（{ctx}）：{e}");
                hidden_by_guard
            }
        },
        WidgetGuardAction::Show => match show() {
            Ok(()) => {
                tracing::info!("全屏守卫恢复悬浮球（{ctx}）");
                false
            }
            Err(e) => {
                tracing::warn!("悬浮球恢复显示失败，下一轮重试（{ctx}）：{e}");
                hidden_by_guard
            }
        },
        WidgetGuardAction::None => hidden_by_guard,
    }
}

/// 组装守卫动作日志的传感器上下文（取证字段：开关 / QUNS 原始值 / 几何结果 /
/// 铺满窗口扫描 / 前台摘要）
fn guard_log_ctx(
    auto_hide: bool,
    probe: &kimicodebar::fullscreen::FullscreenProbe,
    covering: bool,
) -> String {
    let fg = match &probe.foreground {
        Some(f) => format!(
            "{} [{}] ({},{})-({},{})",
            f.exe_name,
            f.class_name,
            f.window_rect.0,
            f.window_rect.1,
            f.window_rect.2,
            f.window_rect.3
        ),
        None => "无前台窗口".to_string(),
    };
    format!(
        "widget_auto_hide={auto_hide} quns={:?} geometry={} covering={covering} 前台={fg}",
        probe.quns, probe.heuristic
    )
}

/// 全屏守卫 ticker：球窗口存在期间每 FULLSCREEN_GUARD_SECS 探测一次，命中 hide、
/// 退出 show；命中/恢复各落一行日志（仿轮询探针行，供「球还在不在」类问题对照）。
/// 循环退出三条路：被新一轮守卫接替（代次不符）、窗口离开 Tauri 窗口表（销毁
/// 完成——此时按最新设置补建检查，是 Destroyed 回调之外的兜底入口，坑 57）、
/// 销毁中僵尸期跳过动作只盯窗口表。
/// 判定用悬浮球专用的 fullscreen::widget_hide_decision_with_covering：稳态矩阵
/// （裸 QUNS=2 不藏）之上补「QUNS=2 + 铺满窗口佐证」——启动/重建后本进程的隐藏
/// 窗口可能被 WebView2 激活成前台且不释放，前台几何结构性失明，靠非前台扫描兜底
/// （与托盘/通知共享的 guard_decision 语义不同，勿混用）。
/// initially_hidden：创建期已按同判定藏住的窗口传 true——守卫状态必须与窗口实际
/// 可见性一致，否则退出全屏后没人发 Show（球藏死）；每轮重读设置，widget_auto_hide
/// 关闭当场生效（被藏的球下一周期恢复，之后守卫不再藏球）
fn spawn_fullscreen_guard(app: AppHandle, initially_hidden: bool) {
    let gen = WIDGET_GUARD_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    tauri::async_runtime::spawn(async move {
        let mut ticker =
            tokio::time::interval(std::time::Duration::from_secs(FULLSCREEN_GUARD_SECS));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut hidden_by_guard = initially_hidden;
        loop {
            ticker.tick().await;
            if WIDGET_GUARD_SEQ.load(Ordering::SeqCst) != gen {
                return; // 窗口重建过：守卫已由新一轮接管
            }
            let Some(window) = app.get_webview_window(WIDGET_LABEL) else {
                // 窗口离开 Tauri 窗口表 = 销毁真正完成（Destroyed 回调是第一补建
                // 入口，这里是兜底：事件丢失/迟到也能在 ≤2 秒内补上）。守卫自身
                // 代次未变 → 没有新窗口接管 → 直接触发补建检查后退出
                tracing::info!("悬浮球窗口已离开窗口表，全屏守卫退出并按最新设置补建检查");
                spawn_reconcile(&app);
                return;
            };
            let (pending, pending_age, timeout_warned) = {
                let lc = lifecycle_lock();
                (
                    lc.destroy_pending,
                    lc.destroy_requested_at.map(|t| t.elapsed()),
                    lc.destroy_timeout_warned,
                )
            };
            if pending {
                // 销毁中僵尸：不做传感器动作（hide/show 会失败刷屏），只盯窗口表
                // 移除 + 超时告警一次（恢复靠兜底分支，不重试销毁）
                let timeout = std::time::Duration::from_secs(DESTROY_CONFIRM_TIMEOUT_SECS);
                if pending_age.is_some_and(|a| a >= timeout) && !timeout_warned {
                    tracing::warn!(
                        "悬浮球销毁确认超时（已 {pending_age:?}，Tauri 窗口表仍残留旧条目）\
                         ——守卫持续监视，条目移除后按最新设置自动补建"
                    );
                    lifecycle_lock().destroy_timeout_warned = true;
                }
                continue;
            }
            let settings = storage::load_settings().unwrap_or_default();
            let probe = kimicodebar::fullscreen::probe();
            let covering = kimicodebar::fullscreen::covering_fullscreen_window_present();
            let should_hide = kimicodebar::fullscreen::widget_hide_decision_with_covering(
                probe.quns,
                probe.heuristic,
                covering,
                settings.widget_auto_hide,
            );
            let action = widget_guard_action(should_hide, hidden_by_guard);
            let ctx = guard_log_ctx(settings.widget_auto_hide, &probe, covering);
            // 每跳探针行（debug 级默认关，RUST_LOG=debug 取证用；info 日志零刷屏）
            tracing::debug!(
                "悬浮球守卫探针：{ctx} → should_hide={should_hide} action={action:?} hidden={hidden_by_guard}"
            );
            hidden_by_guard = apply_guard_action(
                action,
                hidden_by_guard,
                &ctx,
                || window.hide(),
                || window.show(),
            );
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

    // ---- 全屏守卫状态机（传感器矩阵见 fullscreen.rs；此处测动作判定与失败重试）----

    #[test]
    fn guard_action_transitions() {
        use WidgetGuardAction::*;
        // 命中且当前可见 → 藏
        assert_eq!(widget_guard_action(true, false), Hide);
        // 不再命中且当前被守卫藏 → 恢复（含 widget_auto_hide 关闭路径）
        assert_eq!(widget_guard_action(false, true), Show);
        // 持续全屏 / 持续可见：无动作（也就无日志，防每 2 秒刷屏）
        assert_eq!(widget_guard_action(true, true), None);
        assert_eq!(widget_guard_action(false, false), None);
    }

    #[test]
    fn switch_off_in_fullscreen_triggers_restore_next_tick() {
        // 真全屏中（QUNS=2 + 几何命中）关闭开关：判定恒不藏 → 被藏的球下一周期恢复
        let should_hide = kimicodebar::fullscreen::widget_hide_decision(Some(2), true, false);
        assert!(!should_hide);
        assert_eq!(
            widget_guard_action(should_hide, true),
            WidgetGuardAction::Show
        );
    }

    #[test]
    fn hide_show_success_updates_state() {
        // 成功的 hide：状态翻为隐藏；show 分支不应被调用
        assert!(apply_guard_action(
            WidgetGuardAction::Hide,
            false,
            "t",
            || Ok::<(), String>(()),
            || panic!("Hide 分支不应调用 show"),
        ));
        // 成功的 show：状态翻为可见；hide 分支不应被调用
        assert!(!apply_guard_action(
            WidgetGuardAction::Show,
            true,
            "t",
            || panic!("Show 分支不应调用 hide"),
            || Ok::<(), String>(()),
        ));
    }

    #[test]
    fn restore_failure_keeps_hidden_and_retries_next_tick() {
        // 恢复失败：状态保持「被守卫隐藏」（窗口实际不可见，状态必须与窗口一致）
        let hidden = apply_guard_action(
            WidgetGuardAction::Show,
            true,
            "t",
            || Ok::<(), String>(()),
            || Err("模拟 show 失败".to_string()),
        );
        assert!(hidden);
        // 下一轮同输入（should_hide=false、hidden=true）重算出同一动作 Show 重试；
        // 这次成功 → 恢复可见
        assert_eq!(widget_guard_action(false, hidden), WidgetGuardAction::Show);
        assert!(!apply_guard_action(
            widget_guard_action(false, hidden),
            hidden,
            "t",
            || Ok::<(), String>(()),
            || Ok::<(), String>(()),
        ));
    }

    #[test]
    fn hide_failure_keeps_visible_state_for_retry() {
        // 隐藏失败：状态保持「未被守卫隐藏」，下一轮重算出 Hide 再试
        let hidden = apply_guard_action(
            WidgetGuardAction::Hide,
            false,
            "t",
            || Err("模拟 hide 失败".to_string()),
            || Ok::<(), String>(()),
        );
        assert!(!hidden);
        assert_eq!(widget_guard_action(true, hidden), WidgetGuardAction::Hide);
    }

    #[test]
    fn none_action_preserves_state_without_touching_window() {
        // 无动作：窗口 API 一次都不该碰
        let never_hide = || -> Result<(), String> { panic!("None 不应调用 hide") };
        let never_show = || -> Result<(), String> { panic!("None 不应调用 show") };
        assert!(apply_guard_action(
            WidgetGuardAction::None,
            true,
            "t",
            never_hide,
            never_show
        ));
        assert!(!apply_guard_action(
            WidgetGuardAction::None,
            false,
            "t",
            never_hide,
            never_show
        ));
    }

    #[test]
    fn guard_log_ctx_carries_sensor_evidence() {
        // 日志上下文必须带齐取证字段：开关 / QUNS 原始值 / 几何结果 / 铺满扫描 / 前台摘要
        let probe = kimicodebar::fullscreen::FullscreenProbe {
            quns: Some(2),
            heuristic: false,
            foreground: Some(kimicodebar::fullscreen::ForegroundInfo {
                exe_name: "game.exe".to_string(),
                class_name: "UnityWndClass".to_string(),
                window_rect: (0, 0, 1920, 1080),
                monitor_rect: (0, 0, 1920, 1080),
            }),
            guard: true,
        };
        let ctx = guard_log_ctx(true, &probe, true);
        assert!(ctx.contains("widget_auto_hide=true"), "ctx={ctx}");
        assert!(ctx.contains("quns=Some(2)"), "ctx={ctx}");
        assert!(ctx.contains("geometry=false"), "ctx={ctx}");
        assert!(ctx.contains("covering=true"), "ctx={ctx}");
        assert!(ctx.contains("game.exe [UnityWndClass]"), "ctx={ctx}");

        // 无前台窗口（API 失败）也不慌
        let probe = kimicodebar::fullscreen::FullscreenProbe {
            quns: None,
            heuristic: false,
            foreground: None,
            guard: false,
        };
        let ctx = guard_log_ctx(false, &probe, false);
        assert!(ctx.contains("widget_auto_hide=false"), "ctx={ctx}");
        assert!(ctx.contains("quns=None"), "ctx={ctx}");
        assert!(ctx.contains("covering=false"), "ctx={ctx}");
        assert!(ctx.contains("无前台窗口"), "ctx={ctx}");
    }

    #[test]
    fn guard_initial_hidden_state_reconciles_via_action_matrix() {
        // 创建期藏住的球（initially_hidden=true）：守卫初态必须与窗口一致——
        // 退出全屏后（should_hide=false）按动作矩阵发出 Show 恢复；
        // 全屏仍在（should_hide=true）则 None 维持隐藏。反向（初始可见）同理由
        // guard_action_transitions 覆盖
        let still_fullscreen =
            kimicodebar::fullscreen::widget_hide_decision_with_covering(Some(2), false, true, true);
        assert!(still_fullscreen);
        assert_eq!(
            widget_guard_action(still_fullscreen, true),
            WidgetGuardAction::None
        );
        let fullscreen_over = kimicodebar::fullscreen::widget_hide_decision_with_covering(
            Some(5),
            false,
            false,
            true,
        );
        assert!(!fullscreen_over);
        assert_eq!(
            widget_guard_action(fullscreen_over, true),
            WidgetGuardAction::Show
        );
    }

    // ---- 生命周期决策（坑 57：快速关开后消失）----

    #[test]
    fn reconcile_decision_full_matrix() {
        use WidgetReconcileAction::{AwaitDestroyConfirm, Create, Destroy};
        // (enabled, 窗口在表, 销毁中) → 动作
        assert_eq!(widget_reconcile_decision(true, false, false), Create);
        assert_eq!(widget_reconcile_decision(true, false, true), Create); // 确认已到但状态未清也建（窗口不在表即无冲突）
        assert_eq!(
            widget_reconcile_decision(true, true, false),
            WidgetReconcileAction::None // 健在
        );
        assert_eq!(
            widget_reconcile_decision(true, true, true),
            AwaitDestroyConfirm
        ); // 僵尸销毁中
        assert_eq!(widget_reconcile_decision(false, true, false), Destroy);
        assert_eq!(
            widget_reconcile_decision(false, true, true),
            WidgetReconcileAction::None // 已请求销毁，不重复打
        );
        assert_eq!(
            widget_reconcile_decision(false, false, false),
            WidgetReconcileAction::None
        );
        assert_eq!(
            widget_reconcile_decision(false, false, true),
            WidgetReconcileAction::None
        );
    }

    #[test]
    fn reconcile_reenable_while_destroying_waits_then_creates() {
        // 销毁中重开（坑 57 主场景）：旧代码此处无操作 → 僵尸清掉后无补建 → 球
        // 永久消失。新决策：销毁未确认时等 Destroyed 确认，确认后（窗口离开表）
        // 同一输入收敛到 Create
        assert_eq!(
            widget_reconcile_decision(true, true, true),
            WidgetReconcileAction::AwaitDestroyConfirm
        );
        // Destroyed 确认到达：窗口表已清（tauri 先移表项再派发监听器）
        assert_eq!(
            widget_reconcile_decision(true, false, false),
            WidgetReconcileAction::Create
        );
    }

    #[test]
    fn reconcile_stays_closed_never_creates() {
        // 保持关闭：任何输入组合都不产生 Create（不补建）
        assert_eq!(
            widget_reconcile_decision(false, true, false),
            WidgetReconcileAction::Destroy
        );
        assert_eq!(
            widget_reconcile_decision(false, true, true),
            WidgetReconcileAction::None
        );
        assert_eq!(
            widget_reconcile_decision(false, false, false),
            WidgetReconcileAction::None
        );
        assert_eq!(
            widget_reconcile_decision(false, false, true),
            WidgetReconcileAction::None
        );
    }

    #[test]
    fn destroyed_notification_valid_only_for_current_generation() {
        // 当前代次的销毁确认有效（触发补建 reconcile）
        assert!(widget_destroyed_notification(3, 3));
        // 旧代次迟到通知：当前已是新一代窗口 → 忽略，不重建
        assert!(!widget_destroyed_notification(4, 3));
        // 从未创建过（generation=0）→ 无效
        assert!(!widget_destroyed_notification(0, 0));
    }

    #[test]
    fn duplicate_destroyed_notification_does_not_recreate() {
        // 序列模拟：第 1 代销毁 → 确认有效 → 补建出第 2 代 → 旧确认重复送达：
        // 通知代次(1) != 当前代次(2) → 忽略（窗口表里是健在的第 2 代，决策 None）
        assert!(widget_destroyed_notification(1, 1)); // 首次确认有效
        assert!(!widget_destroyed_notification(2, 1)); // 重复通知被代次挡下
        assert_eq!(
            widget_reconcile_decision(true, true, false),
            WidgetReconcileAction::None // 第 2 代健在，不再建
        );
    }
}
