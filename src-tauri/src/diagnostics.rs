//! 诊断导出：汇总版本/系统/设置/凭证状态/最近错误与最近一次 usages 原始响应，
//! 写入 `{config_dir}/diagnostics-YYYYMMDD-HHmmss.txt`。
//!
//! 安全约束：导出内容绝不包含任何 token / Key / Authorization 值；
//! usages 原始响应先经 [`mask_sensitive`] 脱敏（id 类字段替换为 "***"），
//! 解析失败的原文直接不收录。

use std::path::PathBuf;

use kimicodebar::creds;
use kimicodebar::kimi::oauth;
use kimicodebar::storage;

use crate::commands::PanelState;

/// 原始响应收录上限（与 AppState 内存截断一致，超出部分不进诊断）
pub const MAX_RAW_BODY_LEN: usize = 20 * 1024;

/// 生成诊断文本并写入配置目录，返回诊断文件路径
pub fn export(panel: &PanelState, raw_response: Option<(String, i64)>) -> Result<PathBuf, String> {
    let dir = storage::config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建配置目录失败: {e}"))?;
    let now = chrono::Local::now();
    let path = dir.join(format!("diagnostics-{}.txt", now.format("%Y%m%d-%H%M%S")));
    std::fs::write(&path, build_report(panel, raw_response, now))
        .map_err(|e| format!("写入诊断文件失败: {e}"))?;
    Ok(path)
}

/// 递归脱敏：对象中 id 类字段（userId/businessId/subscriptionId 等）的值替换为 "***"
pub fn mask_sensitive(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                if is_sensitive_key(key) {
                    *v = serde_json::Value::String("***".to_string());
                } else {
                    mask_sensitive(v);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                mask_sensitive(item);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// 以下为内部实现
// ---------------------------------------------------------------------------

/// id 类字段判定：显式名单（小写比较）+ `_id` 后缀 + camelCase 的 `XxxId` 后缀
fn is_sensitive_key(key: &str) -> bool {
    /// 常见的 id 类字段名（小写形式，覆盖全小写写法）
    const KNOWN: [&str; 6] = [
        "id",
        "userid",
        "businessid",
        "subscriptionid",
        "deviceid",
        "sessionid",
    ];
    let lower = key.to_ascii_lowercase();
    KNOWN.contains(&lower.as_str()) || lower.ends_with("_id") || key.ends_with("Id")
}

/// 组装诊断报告全文
fn build_report(
    panel: &PanelState,
    raw_response: Option<(String, i64)>,
    now: chrono::DateTime<chrono::Local>,
) -> String {
    let mut out = String::new();
    out.push_str("KimiCodeBar 诊断报告\n");
    out.push_str("====================\n");
    out.push_str(&format!("导出时间: {}\n", now.format("%Y-%m-%d %H:%M:%S")));
    out.push_str(&format!("应用版本: {}\n", env!("CARGO_PKG_VERSION")));
    out.push_str(&format!(
        "操作系统: {} {}\n",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));

    // 设置全文（settings.json 本身不含密钥）
    out.push_str("\n[设置]\n");
    match storage::load_settings() {
        Ok(settings) => match serde_json::to_string_pretty(&settings) {
            Ok(json) => out.push_str(&json),
            Err(e) => out.push_str(&format!("<序列化失败: {e}>")),
        },
        Err(e) => out.push_str(&format!("<读取失败: {e}>")),
    }

    // 凭证只导出 configured 布尔，绝不导出值
    out.push_str("\n\n[凭证状态]\n");
    out.push_str(&format!(
        "api_key_configured: {}\n",
        matches!(creds::load_api_key(), Ok(Some(_)))
    ));
    out.push_str(&format!(
        "oauth_configured: {}\n",
        matches!(oauth::load_credentials(), Ok(Some(_)))
    ));
    out.push_str(&format!(
        "web_token_configured: {}\n",
        matches!(creds::load_web_token(), Ok(Some(_)))
    ));
    out.push_str(&format!(
        "web_refresh_token_configured: {}\n",
        matches!(creds::load_web_refresh_token(), Ok(Some(_)))
    ));

    // 最近一次错误（配额 / 月度）
    out.push_str("\n[最近错误]\n");
    out.push_str(&format!(
        "quota_error: {}\n",
        panel.error.as_deref().unwrap_or("<无>")
    ));
    out.push_str(&format!(
        "monthly_error: {}\n",
        panel.monthly_error.as_deref().unwrap_or("<无>")
    ));

    // 最近一次 usages 原始响应（脱敏后附上）
    out.push_str("\n[最近一次 usages 原始响应]\n");
    match raw_response {
        Some((raw, fetched_at)) => {
            out.push_str(&format!("fetched_at: {fetched_at}\n"));
            out.push_str(&format_raw_body(&raw));
        }
        None => out.push_str("<尚无成功响应>"),
    }
    out.push('\n');
    out
}

/// 原始响应脱敏为 pretty JSON；解析失败的原文可能含未脱敏字段，不收录内容
fn format_raw_body(raw: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(mut value) => {
            mask_sensitive(&mut value);
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| "<序列化失败>".to_string())
        }
        Err(_) => "<响应非 JSON，未收录>".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_replaces_known_id_fields() {
        let mut value = serde_json::json!({
            "userId": "u-123",
            "businessId": 42,
            "subscriptionId": "sub-9",
            "membership_level": "LEVEL_FREE"
        });
        mask_sensitive(&mut value);
        assert_eq!(value["userId"], "***");
        assert_eq!(value["businessId"], "***");
        assert_eq!(value["subscriptionId"], "***");
        // 非 id 字段原样保留
        assert_eq!(value["membership_level"], "LEVEL_FREE");
    }

    #[test]
    fn mask_recurses_into_nested_objects_and_arrays() {
        let mut value = serde_json::json!({
            "usage": {
                "user_id": "u-123",
                "limit": "100",
                "detail": { "device_id": "dev-1", "used": "30" }
            },
            "list": [ { "sessionId": "sess-2" }, { "name": "keep" } ]
        });
        mask_sensitive(&mut value);
        assert_eq!(value["usage"]["user_id"], "***");
        assert_eq!(value["usage"]["detail"]["device_id"], "***");
        assert_eq!(value["list"][0]["sessionId"], "***");
        // 非 id 字段（含数组内）原样保留
        assert_eq!(value["usage"]["limit"], "100");
        assert_eq!(value["usage"]["detail"]["used"], "30");
        assert_eq!(value["list"][1]["name"], "keep");
    }

    #[test]
    fn mask_leaves_scalars_and_idless_objects_untouched() {
        let mut value = serde_json::json!({
            "limit": 100,
            "identity": "not-an-id-field-name-but-ends-with-ty"
        });
        mask_sensitive(&mut value);
        assert_eq!(value["limit"], 100);
        assert_eq!(value["identity"], "not-an-id-field-name-but-ends-with-ty");

        let mut scalar = serde_json::json!("plain");
        mask_sensitive(&mut scalar);
        assert_eq!(scalar, "plain");
    }

    #[test]
    fn format_raw_body_masks_and_pretty_prints() {
        let out = format_raw_body(r#"{"userId":"u-1","usage":{"limit":"100"}}"#);
        assert!(out.contains("\"userId\": \"***\""));
        assert!(out.contains("\"limit\": \"100\""));
    }

    #[test]
    fn format_raw_body_drops_non_json() {
        // 非 JSON 原文可能含未脱敏字段，绝不收录
        assert_eq!(
            format_raw_body("not json userId=abc"),
            "<响应非 JSON，未收录>"
        );
    }
}
