//! 全局热键：用户输入规范化 + 注册/重注册（tauri-plugin-global-shortcut）+ 录制期暂停/恢复。
//!
//! 设置里的热键为空（None/空串）表示禁用；保存设置时先全量注销再按新值注册，
//! 被其他程序占用时返回中文错误"热键注册失败：可能被其他程序占用"。
//! 设置页录制热键期间先 pause 注销（否则已注册的组合被系统拦截，录制框收不到按键），
//! 录制结束由 resume 按已保存设置恢复。

use tauri::AppHandle;
use tauri_plugin_global_shortcut::GlobalShortcutExt;

/// 规范化用户输入的热键为 tauri 全局快捷键格式（如 "Control+Shift+K"）。
///
/// 规则：大小写不敏感；`ctrl` 归一为 `Control`；允许 `CmdOrControl`；
/// 各段以 `+` 分隔、允许两侧空格；必须恰好一个主键且至少一个修饰键。
/// 格式非法返回中文错误。
pub fn normalize_hotkey(input: &str) -> Result<String, String> {
    let mut modifiers: Vec<&'static str> = Vec::new();
    let mut key: Option<String> = None;

    for part in input.split('+') {
        let token = part.trim();
        if token.is_empty() {
            return Err(format!("热键格式非法: {input}（存在空的组合段）"));
        }
        let lower = token.to_ascii_lowercase();
        let modifier = match lower.as_str() {
            "ctrl" | "control" => Some("Control"),
            "shift" => Some("Shift"),
            "alt" => Some("Alt"),
            "cmd" | "command" | "super" | "meta" => Some("Command"),
            "cmdorcontrol" => Some("CmdOrControl"),
            _ => None,
        };
        match modifier {
            Some(m) => {
                if !modifiers.contains(&m) {
                    modifiers.push(m);
                }
            }
            None => {
                if key.is_some() {
                    return Err(format!("热键格式非法: {input}（只能有一个主键）"));
                }
                key = Some(normalize_key(token));
            }
        }
    }

    let Some(key) = key else {
        return Err(format!("热键格式非法: {input}（缺少主键）"));
    };
    if modifiers.is_empty() {
        return Err(format!("热键格式非法: {input}（至少需要一个修饰键）"));
    }

    let normalized = format!("{}+{}", modifiers.join("+"), key);
    // 最终合法性交给插件解析器把关（主键名是否可识别等）
    normalized
        .parse::<tauri_plugin_global_shortcut::Shortcut>()
        .map_err(|_| format!("热键格式非法: {input}（无法识别的按键）"))?;
    Ok(normalized)
}

/// 应用设置里的热键：先全量注销，再按新值注册；None/空串表示禁用。
/// 注册失败（热键被占用）返回中文错误。
pub fn apply(app: &AppHandle, hotkey: Option<&str>) -> Result<(), String> {
    let manager = app.global_shortcut();
    // 全量注销失败不阻断：可能只是本来就没注册过
    if let Err(e) = manager.unregister_all() {
        tracing::warn!("注销旧全局热键失败: {e}");
    }

    let Some(raw) = hotkey.map(str::trim).filter(|h| !h.is_empty()) else {
        tracing::info!("全局热键已禁用");
        return Ok(());
    };

    let normalized = normalize_hotkey(raw)?;
    manager.register(normalized.as_str()).map_err(|_| {
        tracing::warn!("全局热键注册失败（可能被占用）: {normalized}");
        "热键注册失败：可能被其他程序占用".to_string()
    })?;
    tracing::info!("全局热键已注册: {normalized}");
    Ok(())
}

/// 录制热键期间临时注销所有全局热键（录制结束由 resume 恢复）。
/// 注销失败只记日志：可能只是本来就没注册过
pub fn pause(app: &AppHandle) {
    if let Err(e) = app.global_shortcut().unregister_all() {
        tracing::warn!("录制前注销全局热键失败: {e}");
    }
}

/// 录制结束后恢复：按已保存的设置重新注册（未保存的录制值不生效）
pub fn resume(app: &AppHandle) -> Result<(), String> {
    let settings = kimicodebar::storage::load_settings().unwrap_or_default();
    apply(app, settings.hotkey.as_deref())
}

/// 主键段规范化：纯 ASCII 字母数字整体大写（k → K、f4 → F4），其余原样保留
fn normalize_key(token: &str) -> String {
    if token.is_ascii() && token.chars().all(|c| c.is_ascii_alphanumeric()) {
        token.to_ascii_uppercase()
    } else {
        token.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_lowercase_ctrl_shift_k() {
        assert_eq!(normalize_hotkey("ctrl+shift+k").unwrap(), "Control+Shift+K");
    }

    #[test]
    fn normalize_uppercase_is_case_insensitive() {
        assert_eq!(normalize_hotkey("CTRL+SHIFT+K").unwrap(), "Control+Shift+K");
    }

    #[test]
    fn normalize_keeps_cmd_or_control() {
        assert_eq!(
            normalize_hotkey("CmdOrControl+Shift+K").unwrap(),
            "CmdOrControl+Shift+K"
        );
        // 大小写不敏感
        assert_eq!(
            normalize_hotkey("cmdorcontrol+shift+k").unwrap(),
            "CmdOrControl+Shift+K"
        );
    }

    #[test]
    fn normalize_tolerates_spaces_and_dedups_modifiers() {
        assert_eq!(normalize_hotkey("  ctrl + k ").unwrap(), "Control+K");
        assert_eq!(normalize_hotkey("ctrl+ctrl+k").unwrap(), "Control+K");
        // 单字母主键大写化；功能键原名保留
        assert_eq!(normalize_hotkey("alt+f4").unwrap(), "Alt+F4");
    }

    #[test]
    fn normalize_rejects_illegal_input() {
        // 空的组合段
        assert!(normalize_hotkey("ctrl++k").is_err());
        assert!(normalize_hotkey("ctrl+").is_err());
        // 缺少主键（全是修饰键）
        assert!(normalize_hotkey("ctrl+shift").is_err());
        // 缺少修饰键
        assert!(normalize_hotkey("k").is_err());
        // 多个主键
        assert!(normalize_hotkey("ctrl+k+j").is_err());
        // 无法识别的按键
        assert!(normalize_hotkey("ctrl+notakey").is_err());
        // 空串 / 纯空格
        assert!(normalize_hotkey("").is_err());
        assert!(normalize_hotkey("   ").is_err());
    }
}
