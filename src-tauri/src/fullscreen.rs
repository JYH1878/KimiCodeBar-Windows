//! 全屏应用探测：查 Windows 系统通知状态（SHQueryUserNotificationState），
//! 判定游戏全屏（独占 / D3D）/ 演示模式是否活跃。全屏静默守卫（tray 托盘更新、
//! polling 系统通知）据此跳过对外动作，防 5 分钟轮询把用户从全屏应用踢回桌面。

/// 全屏应用是否活跃。API 失败一律 false（fail-open：宁可照常刷新托盘，
/// 也不因探测异常让托盘状态哑火卡死）。
pub fn fullscreen_app_active() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::{
            SHQueryUserNotificationState, QUERY_USER_NOTIFICATION_STATE,
        };
        let mut state: QUERY_USER_NOTIFICATION_STATE = 0;
        // SAFETY：只向本地变量写出一个 i32，无别名指针，调用同步返回
        let hr = unsafe { SHQueryUserNotificationState(&mut state) };
        if hr < 0 {
            return false;
        }
        notification_state_fullscreen(state)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// 系统通知状态 → 是否全屏静默（纯映射，单测锚点）。
/// 2 = QUNS_BUSY（独占全屏/演示设置）、3 = QUNS_RUNNING_D3D_FULL_SCREEN、
/// 4 = QUNS_PRESENTATION_MODE 视为全屏；1/5/6/7（锁屏/正常/免打扰时段/Win11 窗口化全屏）
/// 与未知新枚举值一律 false（未识别的场景不静默，同 fail-open）。
fn notification_state_fullscreen(state: i32) -> bool {
    matches!(state, 2..=4)
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
}
