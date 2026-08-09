//! Kimi 网页端月度总量查询：`GetSubscriptionStats`（Connect-RPC JSON 变体）。
//!
//! 该数据只在网页端接口提供（我们的 OAuth token 调不通，401）。
//!
//! ## 鉴权体系（2026-08 迁移，issue #16）
//!
//! kimi 网页端已废弃 `kimi-auth` cookie，改用短寿命 Bearer token + refresh_token 续期：
//! - 新登录不再签发 kimi-auth cookie；网页版请求只带 `Authorization: Bearer <JWT>`
//!   （access_token 仅 15 分钟有效，payload 含 ssid/sub、无 device_id）；
//! - refresh_token 约 90 天有效，每次续期轮换（旧的立即失效），续期端点为
//!   `auth.kimi.com/api/account.gateway.v1.AuthService/RefreshToken`；
//! - 旧体系 kimi-auth JWT 未过期前仍可作 Bearer 用（双头发送兼容过渡期）。
//!
//! 解析防御思路参考 token-monitor 的 kimiLimits.js：容忍 data 包裹、
//! camelCase/snake_case 别名、ratio ≤1 视为小数（×100），subscriptionBalance
//! 仅在 feature/type 匹配时采信。

use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::kimi::USER_AGENT;

/// 月度总量查询接口（Connect-RPC JSON：POST + `{}` body）
const SUBSCRIPTION_STATS_URL: &str =
    "https://www.kimi.com/apiv2/kimi.gateway.membership.v2.MembershipService/GetSubscriptionStats";
/// 网页端 refresh_token 续期端点（Connect-RPC JSON：POST + `{"refresh_token":...}` body）
const REFRESH_TOKEN_URL: &str =
    "https://auth.kimi.com/api/account.gateway.v1.AuthService/RefreshToken";
/// 网页端请求超时（秒）：比常规 API 略短，避免拖慢刷新主流程
const WEB_TIMEOUT_SECS: u64 = 15;
/// token 格式非法时返回给前端的文案
const INVALID_WEB_TOKEN_MESSAGE: &str = "无法识别的 token 格式，请直接粘贴 refresh_token 的值";

/// 月度总量（与 src/types.ts 的 MonthlyInfo 一一对应，百分比为**已用**语义）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonthlyInfo {
    /// 月度总已用百分比（Kimi + Code 合计）
    pub total_pct: f64,
    /// 其中 Kimi 已用百分比（= total - code 防御计算，不为负）
    pub kimi_pct: f64,
    /// 其中 Code 已用百分比
    pub code_pct: f64,
    /// 月度重置时间（expireTime 原样字符串）；可能缺失
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub reset_time: Option<String>,
}

#[derive(Debug, Error)]
pub enum WebError {
    #[error("网页登录态无效或已过期 (HTTP 401/403)")]
    Unauthorized,
    #[error("网络错误: {0}")]
    Http(String),
    #[error("月度数据解析失败: {0}")]
    Parse(String),
    #[error("续期失败: {0}")]
    Refresh(String),
}

/// 规范化用户粘贴的网页 token。接受：
/// - 裸 token（JWT 三段或其他不透明字符串）
/// - `Bearer xxx` / `Authorization: Bearer xxx`（大小写不敏感）
/// - 含 `kimi-auth=xxx` 的整串 cookie（提取到 `;` 或结尾）
///
/// trim 后仍含空格/换行、含分号但找不到 kimi-auth、或结果为空 → Err。
pub fn normalize_web_token(input: &str) -> Result<String, String> {
    let mut raw = input.trim();
    // 容忍整段被引号包住（从终端/文档复制时常见）
    if raw.len() >= 2 {
        let bytes = raw.as_bytes();
        if (bytes[0] == b'"' && bytes[raw.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[raw.len() - 1] == b'\'')
        {
            raw = raw[1..raw.len() - 1].trim();
        }
    }
    if raw.is_empty() {
        return Err(INVALID_WEB_TOKEN_MESSAGE.to_string());
    }

    // 剥 "Authorization:" / "Bearer " 前缀（可叠加，如 "Authorization: Bearer xxx"）
    let raw = strip_ascii_prefix(raw, "authorization:").trim_start();
    let raw = strip_bearer_prefix(raw);

    // 整串 cookie：提取 kimi-auth= 的值（到 ; 或结尾）
    // to_ascii_lowercase 只改 ASCII 字节，字节偏移不变，可安全回切原串
    if let Some(pos) = raw.to_ascii_lowercase().find("kimi-auth=") {
        let value = &raw[pos + "kimi-auth=".len()..];
        let value = value.split(';').next().unwrap_or("").trim();
        let value = value.trim_matches(|c| c == '"' || c == '\'');
        return if value.is_empty() || value.chars().any(char::is_whitespace) {
            Err(INVALID_WEB_TOKEN_MESSAGE.to_string())
        } else {
            Ok(value.to_string())
        };
    }

    // 含分号但不是 kimi-auth cookie（如整串其他 cookie / curl 命令），
    // 或 trim 后内部仍含空格/换行 → 无法识别
    if raw.contains(';') || raw.chars().any(char::is_whitespace) {
        return Err(INVALID_WEB_TOKEN_MESSAGE.to_string());
    }
    Ok(raw.to_string())
}

/// 调用 GetSubscriptionStats 并解析为 MonthlyInfo。
/// 401/403 → Unauthorized；其他非 2xx / 网络失败 → Http；响应不合预期 → Parse。
pub async fn fetch_subscription_stats(token: &str) -> Result<MonthlyInfo, WebError> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(WEB_TIMEOUT_SECS))
        .user_agent(USER_AGENT)
        // token 走明文 HTTP 不可接受，只允许 HTTPS（与 KimiClient 一致）
        .https_only(true)
        .build()
        .map_err(|e| WebError::Http(e.to_string()))?;

    let mut req = http
        .post(SUBSCRIPTION_STATS_URL)
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
        .header(reqwest::header::COOKIE, format!("kimi-auth={token}"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::ORIGIN, "https://www.kimi.com")
        .header(
            reqwest::header::REFERER,
            "https://www.kimi.com/code/console",
        )
        .header("connect-protocol-version", "1")
        .header("x-msh-platform", "web")
        .body("{}");
    // 三段 JWT 能解出会话标识则带上；解不出就省略（服务端仍可凭 cookie 鉴权）
    for (name, value) in jwt_session_headers(token) {
        req = req.header(name, value);
    }

    let resp = req.send().await.map_err(|e| {
        tracing::warn!("月度总量请求发送失败: {e}");
        WebError::Http(e.to_string())
    })?;
    let status = resp.status();
    let body = resp.text().await.map_err(|e| {
        tracing::warn!("月度总量响应读取失败: {e}");
        WebError::Http(e.to_string())
    })?;

    // 错误分支只记状态码与响应体长度，严禁记录 token / 响应原文
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        tracing::warn!(
            "月度总量接口鉴权失败: status={}, body_len={}",
            status.as_u16(),
            body.len()
        );
        return Err(WebError::Unauthorized);
    }
    if !status.is_success() {
        tracing::warn!(
            "月度总量接口返回非 2xx: status={}, body_len={}",
            status.as_u16(),
            body.len()
        );
        return Err(WebError::Http(format!("HTTP {}", status.as_u16())));
    }
    parse_subscription_stats(&body).map_err(|e| {
        if matches!(e, WebError::Parse(_)) {
            tracing::warn!("月度总量响应解析失败: body_len={}", body.len());
        }
        e
    })
}

/// 解析 GetSubscriptionStats 响应为 MonthlyInfo（纯函数，便于单测）。
///
/// 防御点：
/// - 容忍 `data` 包裹层；
/// - subscriptionBalance 仅在 (feature 空或 FEATURE_OMNI) 且 (type 空或 SUBSCRIPTION) 时采信；
/// - 字段兼容 camelCase 与 snake_case 别名，ratio 支持数字或数字字符串；
/// - ratio ≤1 视为小数（×100 得百分比），>1 原样视为百分数；
/// - kimi_pct = (total - code).max(0.0)（code > total 时 Kimi 部分为 0）。
pub fn parse_subscription_stats(body: &str) -> Result<MonthlyInfo, WebError> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| WebError::Parse(e.to_string()))?;
    // 容忍 data 包裹
    let root = value
        .get("data")
        .filter(|d| d.is_object())
        .unwrap_or(&value);

    let balance = pick_object(root, &["subscriptionBalance", "subscription_balance"])
        .ok_or_else(|| WebError::Parse("响应缺少 subscriptionBalance".to_string()))?;

    // feature / type 只在明确给出非预期值时拒绝采信（缺失视为兼容）
    let feature = pick_str(balance, &["feature"]).unwrap_or("");
    let kind = pick_str(balance, &["type"]).unwrap_or("");
    if !(feature.is_empty() || feature == "FEATURE_OMNI")
        || !(kind.is_empty() || kind == "SUBSCRIPTION")
    {
        return Err(WebError::Parse(format!(
            "订阅余额对象不可采信 (feature={feature}, type={kind})"
        )));
    }

    let total_ratio = pick_ratio(
        balance,
        &[
            "amountUsedRatio",
            "amount_used_ratio",
            "usedRatio",
            "used_ratio",
        ],
    )
    .ok_or_else(|| WebError::Parse("缺少 amountUsedRatio".to_string()))?;
    // code 比例缺失/非法时按 0 处理：Kimi 部分即为总量
    let code_ratio =
        pick_ratio(balance, &["kimiCodeUsedRatio", "kimi_code_used_ratio"]).unwrap_or(0.0);

    let total_pct = ratio_to_pct(total_ratio);
    let code_pct = ratio_to_pct(code_ratio);
    let reset_time = pick_str(balance, &["expireTime", "expire_time"])
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    Ok(MonthlyInfo {
        total_pct,
        kimi_pct: (total_pct - code_pct).max(0.0),
        code_pct,
        reset_time,
    })
}

// ---------------------------------------------------------------------------
// 以下为内部实现
// ---------------------------------------------------------------------------

/// 用 refresh_token 换取新的 access_token + refresh_token（新鉴权体系）。
/// 续期成功后 refresh_token 会轮换（旧的立即失效），调用方必须把新的落盘。
///
/// 401/403 → Unauthorized；其他非 2xx / 网络失败 → Http；响应不合预期 → Refresh。
pub async fn refresh_access_token(refresh_token: &str) -> Result<WebSession, WebError> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(WEB_TIMEOUT_SECS))
        .user_agent(USER_AGENT)
        // refresh_token 走明文 HTTP 不可接受，只允许 HTTPS
        .https_only(true)
        .build()
        .map_err(|e| WebError::Http(e.to_string()))?;

    let body = serde_json::json!({ "refresh_token": refresh_token });
    let resp = http
        .post(REFRESH_TOKEN_URL)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::ORIGIN, "https://www.kimi.com")
        .header(
            reqwest::header::REFERER,
            "https://www.kimi.com/code/console",
        )
        .header("connect-protocol-version", "1")
        .header("x-msh-platform", "web")
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            tracing::warn!("refresh_token 续期请求发送失败: {e}");
            WebError::Http(e.to_string())
        })?;
    let status = resp.status();
    let body = resp.text().await.map_err(|e| {
        tracing::warn!("refresh_token 续期响应读取失败: {e}");
        WebError::Http(e.to_string())
    })?;

    // 错误分支只记状态码与响应体长度，严禁记录 token / 响应原文
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        tracing::warn!(
            "refresh_token 续期鉴权失败: status={}, body_len={}",
            status.as_u16(),
            body.len()
        );
        return Err(WebError::Unauthorized);
    }
    if !status.is_success() {
        tracing::warn!(
            "refresh_token 续期返回非 2xx: status={}, body_len={}",
            status.as_u16(),
            body.len()
        );
        return Err(WebError::Http(format!("HTTP {}", status.as_u16())));
    }
    parse_refresh_response(&body)
}

/// 续期成功返回的会话：access_token（短命，15 分钟级）与轮换后的新 refresh_token
#[derive(Debug, Clone)]
pub struct WebSession {
    pub access_token: String,
    /// 轮换后的新 refresh_token，必须落盘；轮换制下旧值立即失效
    pub refresh_token: String,
}

/// 解析 RefreshToken 响应为 WebSession（纯函数，便于单测）。
/// 容忍 `data` 包裹层；字段兼容 camelCase 与 snake_case 别名。
fn parse_refresh_response(body: &str) -> Result<WebSession, WebError> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| WebError::Refresh(e.to_string()))?;
    // 容忍 data 包裹
    let root = value
        .get("data")
        .filter(|d| d.is_object())
        .unwrap_or(&value)
        .as_object()
        .ok_or_else(|| WebError::Refresh("响应不是 JSON 对象".to_string()))?;

    let access_token = pick_str(root, &["accessToken", "access_token"])
        .filter(|s| !s.is_empty())
        .ok_or_else(|| WebError::Refresh("响应缺少 accessToken".to_string()))?;
    // 轮换制：响应必须带新 refresh_token，缺失即按续期失败处理（旧值已失效）
    let refresh_token = pick_str(root, &["refreshToken", "refresh_token"])
        .filter(|s| !s.is_empty())
        .ok_or_else(|| WebError::Refresh("响应缺少 refreshToken".to_string()))?;

    Ok(WebSession {
        access_token: access_token.to_string(),
        refresh_token: refresh_token.to_string(),
    })
}

/// 从 JWT payload 解出过期时间（Unix 秒）；非三段 JWT / 解析失败 → None。
/// 用于判断 access_token 是否临期（新体系 15 分钟即过期，需提前续期）。
pub fn jwt_exp_secs(token: &str) -> Option<i64> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let payload = base64url_decode(parts[1])
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())?;
    payload
        .get("exp")
        .and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)))
}

/// 只读本地设备 ID：`%USERPROFILE%\.kimi-code\device_id`（与 Kimi Code CLI 共享）。
/// 文件不存在 / 内容为空 / 读失败 → None；只读不创建，避免月度查询产生副作用。
fn read_local_device_id() -> Option<String> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)?;
    let path = home.join(".kimi-code").join("device_id");
    let id = std::fs::read_to_string(path).ok()?;
    let id = id.trim();
    (!id.is_empty()).then(|| id.to_string())
}

/// 从 JWT payload 解出网页端会话头（x-msh-device-id / x-msh-session-id / x-traffic-id）。
/// 新体系 JWT（iss=account-nt）只有 ssid/sub、没有 device_id：
/// device_id 缺失时回退到本地设备 ID（`~/.kimi-code/device_id`，与 CLI 共享），
/// 保证新 token 也能发出 X-Msh-Device-Id。
/// 非三段 JWT / base64 或 JSON 解析失败 / ssid 或 sub 缺失 → 空 vec（省略这三头）。
fn jwt_session_headers(token: &str) -> Vec<(String, String)> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Vec::new();
    }
    let payload = base64url_decode(parts[1])
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    let Some(payload) = payload else {
        return Vec::new();
    };
    let get = |key: &str| {
        payload
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    // device_id：JWT 里没有（新体系）就用本地设备 ID（读 `~/.kimi-code/device_id`，
    // 只读不创建；与 CLI 共享，读不到就省略该头）
    let device_id = get("device_id").or_else(read_local_device_id);
    match (device_id, get("ssid"), get("sub")) {
        (Some(device_id), Some(ssid), Some(sub)) => vec![
            ("x-msh-device-id".to_string(), device_id),
            ("x-msh-session-id".to_string(), ssid),
            ("x-traffic-id".to_string(), sub),
        ],
        _ => Vec::new(),
    }
}

/// base64url 解码（容忍省略的 `=` 填充）；非法字符或长度返回 None。
/// 不引 base64 crate（Cargo.toml 依赖被锁定不可改），实现只有十几行。
fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    let input = input.trim_end_matches('=');
    // mod 4 == 1 不可能合法
    if input.len() % 4 == 1 {
        return None;
    }
    let mut table = [0xFFu8; 256];
    for (i, &b) in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
        .iter()
        .enumerate()
    {
        table[b as usize] = i as u8;
    }
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf = 0u32;
    let mut nbits = 0u32;
    for &b in input.as_bytes() {
        let v = table[b as usize];
        if v == 0xFF {
            return None;
        }
        buf = (buf << 6) | u32::from(v);
        nbits += 6;
        if nbits >= 8 {
            nbits -= 8;
            out.push((buf >> nbits) as u8);
        }
    }
    Some(out)
}

/// 大小写不敏感地剥掉 ASCII 前缀；不匹配时原样返回。
/// （用 get 切片避免前缀长度落在非字符边界时 panic）
fn strip_ascii_prefix<'a>(s: &'a str, prefix: &str) -> &'a str {
    match s.get(..prefix.len()) {
        Some(head) if head.eq_ignore_ascii_case(prefix) => &s[prefix.len()..],
        _ => s,
    }
}

/// 剥 "Bearer" 前缀：仅当其后跟空白时才剥（裸 token 本身可能以 bearer 开头）
fn strip_bearer_prefix(s: &str) -> &str {
    let rest = strip_ascii_prefix(s, "bearer");
    if rest.len() != s.len() && rest.starts_with(char::is_whitespace) {
        rest.trim_start()
    } else {
        s
    }
}

/// 从对象里按候选键名取第一个对象值
fn pick_object<'a>(
    obj: &'a serde_json::Value,
    keys: &[&str],
) -> Option<&'a serde_json::Map<String, serde_json::Value>> {
    keys.iter()
        .find_map(|k| obj.get(k).and_then(|v| v.as_object()))
}

/// 从对象里按候选键名取第一个字符串值
fn pick_str<'a>(
    obj: &'a serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| obj.get(*k).and_then(|v| v.as_str()))
}

/// 从对象里按候选键名取第一个比例值：数字或数字字符串均可，负数视为缺失
fn pick_ratio(obj: &serde_json::Map<String, serde_json::Value>, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|k| {
        let v = obj.get(*k)?;
        let r = v
            .as_f64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))?;
        (r >= 0.0 && r.is_finite()).then_some(r)
    })
}

/// ratio → 百分比：≤1 视为小数（×100），>1 原样视为百分数
fn ratio_to_pct(ratio: f64) -> f64 {
    if ratio <= 1.0 {
        ratio * 100.0
    } else {
        ratio
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- normalize_web_token ----

    #[test]
    fn normalize_accepts_bare_jwt() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ4In0.Zm9vLWJhcg";
        assert_eq!(normalize_web_token(jwt).unwrap(), jwt);
    }

    #[test]
    fn normalize_strips_bearer_prefix() {
        assert_eq!(
            normalize_web_token("Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ4In0.Zm9vLWJhcg").unwrap(),
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ4In0.Zm9vLWJhcg"
        );
        // 大小写不敏感
        assert_eq!(
            normalize_web_token("bearer  abc.def.ghi").unwrap(),
            "abc.def.ghi"
        );
    }

    #[test]
    fn normalize_strips_authorization_bearer_prefix() {
        assert_eq!(
            normalize_web_token("Authorization: Bearer abc.def.ghi").unwrap(),
            "abc.def.ghi"
        );
        assert_eq!(
            normalize_web_token("authorization:  bearer abc.def.ghi").unwrap(),
            "abc.def.ghi"
        );
    }

    #[test]
    fn normalize_extracts_kimi_auth_from_cookie() {
        assert_eq!(
            normalize_web_token("kimi-auth=abc.def.ghi; other=1").unwrap(),
            "abc.def.ghi"
        );
        // kimi-auth 不在开头
        assert_eq!(
            normalize_web_token("session=xyz; kimi-auth=tok-en_123; theme=dark").unwrap(),
            "tok-en_123"
        );
        // 值到结尾（无分号）
        assert_eq!(normalize_web_token("kimi-auth=abc").unwrap(), "abc");
    }

    #[test]
    fn normalize_strips_surrounding_quotes() {
        assert_eq!(
            normalize_web_token("\"abc.def.ghi\"").unwrap(),
            "abc.def.ghi"
        );
        assert_eq!(normalize_web_token("'abc.def.ghi'").unwrap(), "abc.def.ghi");
    }

    #[test]
    fn normalize_rejects_blank() {
        assert_eq!(
            normalize_web_token("").unwrap_err(),
            INVALID_WEB_TOKEN_MESSAGE
        );
        assert_eq!(
            normalize_web_token("   \n ").unwrap_err(),
            INVALID_WEB_TOKEN_MESSAGE
        );
        assert_eq!(
            normalize_web_token("\"\"").unwrap_err(),
            INVALID_WEB_TOKEN_MESSAGE
        );
    }

    #[test]
    fn normalize_rejects_inner_whitespace() {
        assert_eq!(
            normalize_web_token("abc def").unwrap_err(),
            INVALID_WEB_TOKEN_MESSAGE
        );
        assert_eq!(
            normalize_web_token("abc\ndef").unwrap_err(),
            INVALID_WEB_TOKEN_MESSAGE
        );
    }

    #[test]
    fn normalize_rejects_cookie_without_kimi_auth() {
        // 整串其他 cookie / 带分号的输入无法识别
        assert_eq!(
            normalize_web_token("session=xyz; theme=dark").unwrap_err(),
            INVALID_WEB_TOKEN_MESSAGE
        );
        // kimi-auth 值为空
        assert_eq!(
            normalize_web_token("kimi-auth=; other=1").unwrap_err(),
            INVALID_WEB_TOKEN_MESSAGE
        );
    }

    // ---- jwt_session_headers ----

    /// 测试用 base64url 编码（无填充）
    fn b64url_encode(data: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let b0 = chunk[0];
            let b1 = chunk.get(1).copied().unwrap_or(0);
            let b2 = chunk.get(2).copied().unwrap_or(0);
            let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
            out.push(ALPHABET[(n >> 18) as usize & 63] as char);
            out.push(ALPHABET[(n >> 12) as usize & 63] as char);
            if chunk.len() > 1 {
                out.push(ALPHABET[(n >> 6) as usize & 63] as char);
            }
            if chunk.len() > 2 {
                out.push(ALPHABET[n as usize & 63] as char);
            }
        }
        out
    }

    fn make_jwt(payload_json: &str) -> String {
        format!(
            "{}.{}.sig",
            b64url_encode(b"{}"),
            b64url_encode(payload_json.as_bytes())
        )
    }

    #[test]
    fn jwt_headers_extracted_from_valid_jwt() {
        let jwt = make_jwt(r#"{"device_id":"dev-1","ssid":"sess-2","sub":"user-3"}"#);
        let headers = jwt_session_headers(&jwt);
        assert_eq!(
            headers,
            vec![
                ("x-msh-device-id".to_string(), "dev-1".to_string()),
                ("x-msh-session-id".to_string(), "sess-2".to_string()),
                ("x-traffic-id".to_string(), "user-3".to_string()),
            ]
        );
    }

    #[test]
    fn jwt_headers_empty_for_two_part_token() {
        assert!(jwt_session_headers("abc.def").is_empty());
        assert!(jwt_session_headers("opaque-token").is_empty());
    }

    #[test]
    fn jwt_headers_empty_for_bad_base64_payload() {
        assert!(jwt_session_headers("aaa.!!!.ccc").is_empty());
        // base64 合法但 payload 不是 JSON
        assert!(jwt_session_headers("aaa.bm90LWpzb24.ccc").is_empty());
    }

    #[test]
    fn jwt_headers_empty_when_any_field_missing() {
        // 缺 ssid / sub：按约定整体省略三头
        let jwt = make_jwt(r#"{"device_id":"dev-1"}"#);
        assert!(jwt_session_headers(&jwt).is_empty());
    }

    #[test]
    fn jwt_headers_new_token_falls_back_to_local_device_id() {
        // 新鉴权体系 JWT 只有 ssid/sub、无 device_id：回退本地设备 ID 补 X-Msh-Device-Id。
        // 用临时 USERPROFILE 写入 device_id 文件，避免依赖真实环境（进程级全局状态持锁串行）
        use crate::TEST_ENV_LOCK as ENV_LOCK;
        let _guard = ENV_LOCK.lock().unwrap();
        let dir =
            std::env::temp_dir().join(format!("kimicodebar-web-deviceid-{}", uuid::Uuid::new_v4()));
        let kimi_dir = dir.join(".kimi-code");
        std::fs::create_dir_all(&kimi_dir).unwrap();
        std::fs::write(kimi_dir.join("device_id"), "local-dev-9").unwrap();
        std::env::set_var("USERPROFILE", &dir);

        let jwt = make_jwt(r#"{"ssid":"sess-2","sub":"user-3"}"#);
        let headers = jwt_session_headers(&jwt);
        assert_eq!(
            headers,
            vec![
                ("x-msh-device-id".to_string(), "local-dev-9".to_string()),
                ("x-msh-session-id".to_string(), "sess-2".to_string()),
                ("x-traffic-id".to_string(), "user-3".to_string()),
            ]
        );

        std::env::remove_var("USERPROFILE");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_local_device_id_missing_home_returns_none() {
        // USERPROFILE 指向不存在目录 → None（只读不创建）
        use crate::TEST_ENV_LOCK as ENV_LOCK;
        let _guard = ENV_LOCK.lock().unwrap();
        let dir =
            std::env::temp_dir().join(format!("kimicodebar-web-nohome-{}", uuid::Uuid::new_v4()));
        std::env::set_var("USERPROFILE", &dir);
        assert!(read_local_device_id().is_none());
        std::env::remove_var("USERPROFILE");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- jwt_exp_secs ----

    #[test]
    fn jwt_exp_parsed_from_valid_token() {
        let jwt = make_jwt(r#"{"exp":1800000000,"ssid":"s"}"#);
        assert_eq!(jwt_exp_secs(&jwt), Some(1_800_000_000));
    }

    #[test]
    fn jwt_exp_none_for_non_jwt_or_missing_exp() {
        assert_eq!(jwt_exp_secs("opaque-token"), None);
        assert_eq!(jwt_exp_secs("abc.def"), None);
        assert_eq!(jwt_exp_secs(&make_jwt(r#"{"ssid":"s"}"#)), None);
        // payload 不是 JSON / base64 非法
        assert_eq!(jwt_exp_secs("aaa.!!!.ccc"), None);
    }

    // ---- parse_refresh_response ----

    #[test]
    fn refresh_response_parses_camel_case() {
        let body = r#"{"accessToken":"at-1","refreshToken":"rt-2"}"#;
        let s = parse_refresh_response(body).unwrap();
        assert_eq!(s.access_token, "at-1");
        assert_eq!(s.refresh_token, "rt-2");
    }

    #[test]
    fn refresh_response_parses_data_wrapped() {
        let body = r#"{"data":{"accessToken":"at-1","refreshToken":"rt-2"}}"#;
        let s = parse_refresh_response(body).unwrap();
        assert_eq!(s.access_token, "at-1");
        assert_eq!(s.refresh_token, "rt-2");
    }

    #[test]
    fn refresh_response_missing_refresh_token_is_error() {
        // 轮换制：响应缺新 refresh_token 按续期失败处理
        let body = r#"{"accessToken":"at-1"}"#;
        assert!(matches!(
            parse_refresh_response(body).unwrap_err(),
            WebError::Refresh(_)
        ));
        let body = r#"{"accessToken":"","refreshToken":""}"#;
        assert!(matches!(
            parse_refresh_response(body).unwrap_err(),
            WebError::Refresh(_)
        ));
        assert!(matches!(
            parse_refresh_response("not json").unwrap_err(),
            WebError::Refresh(_)
        ));
    }

    // ---- parse_subscription_stats ----

    #[test]
    fn parse_data_wrapped_variant() {
        let json = r#"{"data":{"subscriptionBalance":{
            "feature":"FEATURE_OMNI","type":"SUBSCRIPTION",
            "amountUsedRatio":0.25,"kimiCodeUsedRatio":0.05,
            "expireTime":"2026-08-01T00:00:00Z"}}}"#;
        let m = parse_subscription_stats(json).unwrap();
        assert!((m.total_pct - 25.0).abs() < 1e-9);
        assert!((m.code_pct - 5.0).abs() < 1e-9);
        assert!((m.kimi_pct - 20.0).abs() < 1e-9);
    }

    #[test]
    fn parse_percent_ratio_over_one_kept_as_is() {
        // >1 视为已是百分数，原样采用
        let json = r#"{"subscriptionBalance":{"amountUsedRatio":16.12,"kimiCodeUsedRatio":5}}"#;
        let m = parse_subscription_stats(json).unwrap();
        assert!((m.total_pct - 16.12).abs() < 1e-9);
        assert!((m.code_pct - 5.0).abs() < 1e-9);
        assert!((m.kimi_pct - 11.12).abs() < 1e-9);
    }

    #[test]
    fn parse_used_ratio_alias_and_string_numbers() {
        // usedRatio 别名 + 字符串形式的数字
        let json = r#"{"subscriptionBalance":{"usedRatio":"0.2","kimiCodeUsedRatio":"0.05"}}"#;
        let m = parse_subscription_stats(json).unwrap();
        assert!((m.total_pct - 20.0).abs() < 1e-9);
        assert!((m.kimi_pct - 15.0).abs() < 1e-9);
    }

    #[test]
    fn parse_rejects_untrusted_feature_and_type() {
        let err = parse_subscription_stats(
            r#"{"subscriptionBalance":{"feature":"FEATURE_OTHER","amountUsedRatio":0.1}}"#,
        )
        .unwrap_err();
        assert!(matches!(err, WebError::Parse(_)));
        let err = parse_subscription_stats(
            r#"{"subscriptionBalance":{"type":"ADDON","amountUsedRatio":0.1}}"#,
        )
        .unwrap_err();
        assert!(matches!(err, WebError::Parse(_)));
    }

    #[test]
    fn parse_code_over_total_clamps_kimi_to_zero() {
        let json = r#"{"subscriptionBalance":{"amountUsedRatio":0.05,"kimiCodeUsedRatio":0.1612}}"#;
        let m = parse_subscription_stats(json).unwrap();
        assert!((m.total_pct - 5.0).abs() < 1e-9);
        assert!((m.code_pct - 16.12).abs() < 1e-9);
        assert_eq!(m.kimi_pct, 0.0);
    }

    #[test]
    fn parse_missing_code_ratio_defaults_to_zero() {
        let json = r#"{"subscriptionBalance":{"amountUsedRatio":0.3}}"#;
        let m = parse_subscription_stats(json).unwrap();
        assert!((m.total_pct - 30.0).abs() < 1e-9);
        assert_eq!(m.code_pct, 0.0);
        assert!((m.kimi_pct - 30.0).abs() < 1e-9);
        assert!(m.reset_time.is_none());
    }

    #[test]
    fn parse_missing_balance_or_ratio_is_parse_error() {
        assert!(matches!(
            parse_subscription_stats(r#"{"ratelimitCode5h":{}}"#).unwrap_err(),
            WebError::Parse(_)
        ));
        assert!(matches!(
            parse_subscription_stats(r#"{"subscriptionBalance":{"feature":"FEATURE_OMNI"}}"#)
                .unwrap_err(),
            WebError::Parse(_)
        ));
        assert!(matches!(
            parse_subscription_stats("not json").unwrap_err(),
            WebError::Parse(_)
        ));
    }
}
