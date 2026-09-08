//! 全屏应用探测（双传感器取并集，任一命中即静默）：
//! ① SHQueryUserNotificationState：独占全屏 / D3D 全屏 / 演示模式
//!    （Win11 对无边框窗口化全屏常报 NOT_PRESENT，单靠它会漏，故加 ②）；
//! ② 前台窗口几何启发式：前台窗口矩形覆盖其所在显示器（边沿容差 2px）、
//!    且不属于已知外壳类排除名单（桌面 / 任务栏 / 开始菜单等常驻满屏窗口）、
//!    也不是我们自己的窗口（按进程 id 排除）。
//! 托盘更新与系统通知据此静默，防 5 分钟轮询把用户从全屏应用踢回桌面。
//! 所有 API 失败各自按 false（fail-open：宁可照常刷新托盘，也不让守卫哑火卡死）。

/// 矩形覆盖判定的边沿容差（物理像素）：无边框窗口化全屏常与屏幕齐边或大 1px，
/// 内缩超过 2px 视为「没铺满」，不算全屏
const COVER_TOLERANCE_PX: i32 = 2;

/// 单次全屏探测快照（取证用：发送时随探针行落日志，记录判定过程与原始值）。
#[derive(Debug, Clone)]
pub struct FullscreenProbe {
    /// SHQueryUserNotificationState 原始状态值（None = API 调用失败）
    pub quns: Option<i32>,
    /// 前台窗口信息（无前台窗口时为 None；矩形读取失败时矩形字段为全 0）
    pub foreground: Option<ForegroundInfo>,
    /// 几何启发式判定结果：前台窗口覆盖所在显示器且不在排除名单
    pub heuristic: bool,
    /// 最终守卫结论：QUNS ∪ 启发式，任一命中即判全屏
    pub guard: bool,
}

/// 前台窗口信息（埋点取证用）
#[derive(Debug, Clone)]
pub struct ForegroundInfo {
    /// 进程 exe 文件名（取不到为空串）
    pub exe_name: String,
    /// 窗口类名（取不到为空串）
    pub class_name: String,
    /// 窗口矩形 (left, top, right, bottom)，屏幕物理像素坐标
    pub window_rect: (i32, i32, i32, i32),
    /// 所在显示器矩形 (left, top, right, bottom)
    pub monitor_rect: (i32, i32, i32, i32),
}

/// 全屏应用是否活跃（守卫主入口）：QUNS ∪ 几何启发式并集。
/// 各 API 失败一律按 false（fail-open：宁可照常刷新托盘，
/// 也不因探测异常让托盘状态哑火卡死）。
pub fn fullscreen_app_active() -> bool {
    probe().guard
}

/// 取一次完整探测快照（守卫结论 + 埋点取证字段）
pub fn probe() -> FullscreenProbe {
    let (quns, foreground, heuristic) = platform_probe();
    FullscreenProbe {
        quns,
        foreground,
        heuristic,
        guard: guard_decision(quns, heuristic),
    }
}

/// 系统通知状态 → 是否全屏静默（纯映射，单测锚点）。
/// 2 = QUNS_BUSY（独占全屏/演示设置）、3 = QUNS_RUNNING_D3D_FULL_SCREEN、
/// 4 = QUNS_PRESENTATION_MODE 视为全屏；1/5/6/7（锁屏/正常/免打扰时段/Win11 窗口化全屏）
/// 与未知新枚举值一律 false（未识别的场景不静默，同 fail-open）。
fn notification_state_fullscreen(state: i32) -> bool {
    matches!(state, 2..=4)
}

/// 守卫并集判定（纯函数）：QUNS 报全屏或几何启发式命中，任一即静默；
/// QUNS 查询失败（None）时只靠启发式
fn guard_decision(quns: Option<i32>, heuristic: bool) -> bool {
    quns.is_some_and(notification_state_fullscreen) || heuristic
}

/// 矩形覆盖判定（纯函数）：窗口矩形覆盖显示器矩形，四边各允许
/// COVER_TOLERANCE_PX 的内缩容差（窗口比显示器大 / 出屏都算覆盖）
pub(crate) fn rect_covers(win: (i32, i32, i32, i32), mon: (i32, i32, i32, i32)) -> bool {
    win.0 <= mon.0 + COVER_TOLERANCE_PX
        && win.1 <= mon.1 + COVER_TOLERANCE_PX
        && win.2 >= mon.2 - COVER_TOLERANCE_PX
        && win.3 >= mon.3 - COVER_TOLERANCE_PX
}

/// 已知外壳类名排除表（纯函数）：桌面 / 任务栏 / 开始菜单等常驻满屏或近满屏的
/// 系统窗口不算全屏，否则焦点落在桌面时守卫会误命中。
/// Windows 窗口类名不区分大小写，按 ASCII 大小写不敏感比对
pub(crate) fn shell_class_excluded(class_name: &str) -> bool {
    const EXCLUDED: &[&str] = &[
        "Progman",                      // 桌面
        "WorkerW",                      // 桌面工作区（动态壁纸也挂在此类下）
        "Shell_TrayWnd",                // 主任务栏
        "Shell_SecondaryTrayWnd",       // 副屏任务栏
        "NotifyIconOverflowWindow",     // 通知区溢出弹层
        "Windows.UI.Core.CoreWindow",   // 开始菜单 / 任务视图 / 操作中心等 UWP 外壳
        "XamlExplorerHostIslandWindow", // Win11 开始菜单宿主
    ];
    EXCLUDED.iter().any(|c| c.eq_ignore_ascii_case(class_name))
}

#[cfg(windows)]
fn platform_probe() -> (Option<i32>, Option<ForegroundInfo>, bool) {
    let quns = query_notification_state();
    let (foreground, heuristic) = query_foreground();
    (quns, foreground, heuristic)
}

#[cfg(not(windows))]
fn platform_probe() -> (Option<i32>, Option<ForegroundInfo>, bool) {
    (None, None, false)
}

/// QUNS 原始状态值；API 失败（HRESULT < 0）返回 None
#[cfg(windows)]
fn query_notification_state() -> Option<i32> {
    use windows_sys::Win32::UI::Shell::SHQueryUserNotificationState;
    let mut state: i32 = 0;
    // SAFETY：只向本地变量写出一个 i32，无别名指针，调用同步返回
    let hr = unsafe { SHQueryUserNotificationState(&mut state) };
    (hr >= 0).then_some(state)
}

/// 前台窗口探测：返回窗口信息（供埋点日志）与几何启发式判定结果。
/// 任一步 API 失败：信息尽力保留（失败字段给空串/全 0），判定按 false（fail-open）
#[cfg(windows)]
fn query_foreground() -> (Option<ForegroundInfo>, bool) {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId,
    };

    // SAFETY：GetForegroundWindow 无参数、同步返回；NULL 表示当前没有前台窗口
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_null() {
        return (None, false);
    }

    // 进程 id：我们自己的窗口永不算全屏（面板/设置窗最大化不该静默自己）
    let mut pid: u32 = 0;
    // SAFETY：hwnd 非空；&mut pid 指向有效栈变量，失败时 pid 保持 0（≠ 本进程 id）
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    let own_window = pid == std::process::id();
    let exe_name = process_exe_name(pid);

    // 窗口类名（排除桌面/任务栏等常驻满屏外壳窗口）
    let mut class_buf = [0u16; 256];
    // SAFETY：缓冲区长度与容量一致传入；返回值为实际写入的字符数（不含 NUL）
    let class_len = unsafe { GetClassNameW(hwnd, class_buf.as_mut_ptr(), class_buf.len() as i32) };
    let class_name = if class_len > 0 {
        String::from_utf16_lossy(&class_buf[..class_len as usize])
    } else {
        String::new()
    };

    // 窗口矩形（窗口恰好销毁/cloaked 等竞态下可能失败：按未覆盖 fail-open，
    // 但 exe/class 仍进埋点日志）
    // SAFETY：&mut wrect 指向有效栈变量；返回值 0 = 失败
    let mut wrect: RECT = unsafe { std::mem::zeroed() };
    let rect_ok = unsafe { GetWindowRect(hwnd, &mut wrect) } != 0;
    let window_rect = (wrect.left, wrect.top, wrect.right, wrect.bottom);

    // 所在显示器矩形（MONITOR_DEFAULTTONEAREST：跨屏/句柄失效也给最近显示器）
    let mut monitor_rect = (0, 0, 0, 0);
    let mut mon_ok = false;
    if rect_ok {
        let mut mi: MONITORINFO = unsafe { std::mem::zeroed() };
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        // SAFETY：mi 已按 cbSize 正确初始化；MonitorFromWindow 对任意 hwnd 安全
        let hmon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        mon_ok = unsafe { GetMonitorInfoW(hmon, &mut mi) } != 0;
        if mon_ok {
            monitor_rect = (
                mi.rcMonitor.left,
                mi.rcMonitor.top,
                mi.rcMonitor.right,
                mi.rcMonitor.bottom,
            );
        }
    }

    let heuristic = rect_ok
        && mon_ok
        && !own_window
        && !shell_class_excluded(&class_name)
        && rect_covers(window_rect, monitor_rect);

    (
        Some(ForegroundInfo {
            exe_name,
            class_name,
            window_rect,
            monitor_rect,
        }),
        heuristic,
    )
}

/// 按进程 id 取 exe 文件名（埋点日志用；任何一步失败返回空串，fail-open）
#[cfg(windows)]
fn process_exe_name(pid: u32) -> String {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY：只申请 QUERY_LIMITED_INFORMATION 只读权限；句柄非空即用即关
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return String::new();
    }
    let mut buf = [0u16; 260];
    let mut len = buf.len() as u32;
    // SAFETY：缓冲区容量经 len 传入，成功后 len 为实际字符数（不含 NUL）
    let ok = unsafe { QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut len) };
    unsafe { CloseHandle(handle) };
    if ok == 0 {
        return String::new();
    }
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    path.rsplit('\\').next().unwrap_or(&path).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fullscreen_states_map_true() {
        // QUNS_BUSY / QUNS_RUNNING_D3D_FULL_SCREEN / QUNS_PRESENTATION_MODE → 静默
        assert!(notification_state_fullscreen(2));
        assert!(notification_state_fullscreen(3));
        assert!(notification_state_fullscreen(4));
    }

    #[test]
    fn non_fullscreen_states_map_false() {
        // QUNS_NOT_PRESENT / ACCEPTS_NOTIFICATIONS / QUIET_TIME / QUNS_APP（Win11）→ 不静默
        assert!(!notification_state_fullscreen(1));
        assert!(!notification_state_fullscreen(5));
        assert!(!notification_state_fullscreen(6));
        assert!(!notification_state_fullscreen(7));
        // 未知/异常取值（0 与未来新枚举）按不静默
        assert!(!notification_state_fullscreen(0));
        assert!(!notification_state_fullscreen(8));
        assert!(!notification_state_fullscreen(-1));
    }

    // ---- 几何启发式：矩形覆盖判定 ----

    #[test]
    fn rect_cover_exact_and_oversized() {
        // 精确齐边
        assert!(rect_covers((0, 0, 1920, 1080), (0, 0, 1920, 1080)));
        // 比显示器大 1px（无边框窗口化全屏常见的边框补偿）
        assert!(rect_covers((-1, -1, 1921, 1081), (0, 0, 1920, 1080)));
        // 副屏负坐标
        assert!(rect_covers((-1920, 0, 0, 1080), (-1920, 0, 0, 1080)));
    }

    #[test]
    fn rect_cover_tolerance_boundary() {
        // 四边各内缩 2px：容差内 → 覆盖
        assert!(rect_covers((2, 2, 1918, 1078), (0, 0, 1920, 1080)));
        // 任一边内缩 3px：超出容差 → 不算全屏
        assert!(!rect_covers((3, 0, 1920, 1080), (0, 0, 1920, 1080)));
        assert!(!rect_covers((0, 3, 1920, 1080), (0, 0, 1920, 1080)));
        assert!(!rect_covers((0, 0, 1917, 1080), (0, 0, 1920, 1080)));
        assert!(!rect_covers((0, 0, 1920, 1077), (0, 0, 1920, 1080)));
    }

    #[test]
    fn rect_not_covering() {
        // 半屏窗口
        assert!(!rect_covers((0, 0, 960, 1080), (0, 0, 1920, 1080)));
        // 相邻显示器上的窗口（坐标不重叠）
        assert!(!rect_covers((1920, 0, 3840, 1080), (0, 0, 1920, 1080)));
        // 常规最大化窗口（让出任务栏 40px）
        assert!(!rect_covers((0, 0, 1920, 1040), (0, 0, 1920, 1080)));
    }

    // ---- 几何启发式：外壳类名排除表 ----

    #[test]
    fn shell_classes_excluded_case_insensitive() {
        for class in [
            "Progman",
            "WorkerW",
            "Shell_TrayWnd",
            "Shell_SecondaryTrayWnd",
            "NotifyIconOverflowWindow",
            "Windows.UI.Core.CoreWindow",
            "XamlExplorerHostIslandWindow",
        ] {
            assert!(shell_class_excluded(class), "{class} 应在排除表");
        }
        // 窗口类名不区分大小写
        assert!(shell_class_excluded("progman"));
        assert!(shell_class_excluded("SHELL_TRAYWND"));
        // 游戏 / 浏览器 / 普通窗口不排除
        assert!(!shell_class_excluded("UnityWndClass"));
        assert!(!shell_class_excluded("Chrome_WidgetWin_1"));
        assert!(!shell_class_excluded(""));
    }

    // ---- 守卫并集 ----

    #[test]
    fn guard_is_union_of_sensors() {
        // QUNS 报全屏：启发式 false 也静默
        assert!(guard_decision(Some(3), false));
        // QUNS 正常但几何启发式命中（Win11 无边框全屏场景）：静默
        assert!(guard_decision(Some(1), true));
        // 双 false 不静默
        assert!(!guard_decision(Some(1), false));
        // QUNS API 失败（None）：只靠启发式，fail-open
        assert!(!guard_decision(None, false));
        assert!(guard_decision(None, true));
    }
}
