//! OAuth 2.0 Device Code Flow（RFC 8628），与 Kimi Code CLI / Mac 版一致。
//! 参考实现：`KimiCodeBar-Mac/macOS/KimiCodeBar/KimiOAuthService.swift`。

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::kimi::dpapi;
use crate::kimi::{AUTH_BASE, HTTP_TIMEOUT_SECS, OAUTH_CLIENT_ID, USER_AGENT};

/// 轮询总预算 15 分钟，与 CLI / Mac 版一致
const POLL_TIMEOUT_SECS: u64 = 15 * 60;
/// 服务端未给 interval 时的默认轮询间隔（秒）
const DEFAULT_POLL_INTERVAL_SECS: u64 = 5;
/// 收到 slow_down 后间隔增量（秒）
const SLOW_DOWN_INCREMENT_SECS: u64 = 5;

const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";

fn device_authorization_url() -> String {
    format!("{AUTH_BASE}/api/oauth/device_authorization")
}

fn token_url() -> String {
    format!("{AUTH_BASE}/api/oauth/token")
}

#[derive(Debug, Error)]
pub enum OAuthError {
    #[error("网络错误: {0}")]
    Http(String),
    #[error("授权服务错误: {0}")]
    Api(String),
    #[error("设备码已过期，请重新发起登录")]
    Expired,
    #[error("用户拒绝了授权")]
    Denied,
    #[error("授权已被吊销，请重新登录")]
    NotAuthorized,
    #[error("本地 IO 错误: {0}")]
    Io(String),
}

/// 本地持久化的凭证（snake_case JSON，与 Mac 版 credentials.json 对齐）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// 过期时间（Unix 秒）
    #[serde(default)]
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub token_type: Option<String>,
}

/// 设备授权发起结果（展示 user_code 并打开 verification_uri_complete）。
/// 两个 URI 字段在 start_device_auth 已做 https + kimi.com 白名单校验
#[derive(Debug, Clone)]
pub struct DeviceAuthInfo {
    pub user_code: String,
    pub device_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    pub interval: u64,
}

/// POST {AUTH_BASE}/api/oauth/device_authorization
pub async fn start_device_auth() -> Result<DeviceAuthInfo, OAuthError> {
    let client = http_client()?;
    let (status, body) = post_form(
        &client,
        &device_authorization_url(),
        &[("client_id", OAUTH_CLIENT_ID)],
    )
    .await?;

    if !status.is_success() {
        return Err(OAuthError::Api(
            extract_error_message(&body).unwrap_or_else(|| format!("HTTP {status}")),
        ));
    }

    let resp: DeviceAuthResponse = serde_json::from_str(&body)
        .map_err(|_| OAuthError::Api("授权服务返回了无法解析的响应".into()))?;
    if resp.user_code.is_empty() || resp.device_code.is_empty() {
        return Err(OAuthError::Api("授权服务返回了无效的响应".into()));
    }
    let verification_uri = resp.verification_uri.unwrap_or_default();
    let verification_uri_complete = resp.verification_uri_complete;
    // 验证地址是远端可控字符串，点击会在系统浏览器打开：必须 https 且落在
    // kimi.com（含子域），否则按授权服务异常处理（挡住 TLS 中间人的钓鱼跳转）
    for uri in [
        verification_uri.as_str(),
        verification_uri_complete.as_deref().unwrap_or(""),
    ] {
        if !uri.is_empty() && !is_trusted_verification_uri(uri) {
            return Err(OAuthError::Api("授权服务返回了非预期的验证地址".into()));
        }
    }
    Ok(DeviceAuthInfo {
        user_code: resp.user_code,
        device_code: resp.device_code,
        verification_uri,
        verification_uri_complete,
        expires_in: resp.expires_in.unwrap_or(0),
        interval: resp.interval.unwrap_or(0),
    })
}

/// 轮询 {AUTH_BASE}/api/oauth/token 直至拿到 token / 过期 / 拒绝。
/// 初始间隔用 info.interval（缺省 5s），总预算 15 分钟；
/// authorization_pending 继续、slow_down +5s、expired_token → Expired、access_denied → Denied。
pub async fn poll_device_token(info: &DeviceAuthInfo) -> Result<Credentials, OAuthError> {
    let client = http_client()?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(POLL_TIMEOUT_SECS);
    let mut interval_secs = if info.interval == 0 {
        DEFAULT_POLL_INTERVAL_SECS
    } else {
        info.interval
    };

    while tokio::time::Instant::now() < deadline {
        let (status, body) = post_form(
            &client,
            &token_url(),
            &[
                ("client_id", OAUTH_CLIENT_ID),
                ("device_code", &info.device_code),
                ("grant_type", DEVICE_GRANT_TYPE),
            ],
        )
        .await?;

        if status.is_success() {
            return credentials_from_token_body(&body, None);
        }

        match classify_poll_error(extract_error_code(&body).as_deref()) {
            PollAction::Pending => {}
            PollAction::SlowDown => interval_secs += SLOW_DOWN_INCREMENT_SECS,
            PollAction::Expired => return Err(OAuthError::Expired),
            PollAction::Denied => return Err(OAuthError::Denied),
            PollAction::Api => {
                return Err(OAuthError::Api(
                    extract_error_message(&body).unwrap_or_else(|| format!("HTTP {status}")),
                ));
            }
        }

        tokio::time::sleep(Duration::from_secs(interval_secs)).await;
    }

    // 总预算耗尽：设备码等效于已过期
    Err(OAuthError::Expired)
}

/// 用 refresh_token 换新 token；invalid_grant → NotAuthorized。
pub async fn refresh_token(creds: &Credentials) -> Result<Credentials, OAuthError> {
    let old_refresh = creds
        .refresh_token
        .as_deref()
        .filter(|t| !t.is_empty())
        .ok_or(OAuthError::NotAuthorized)?;

    let client = http_client()?;
    let (status, body) = post_form(
        &client,
        &token_url(),
        &[
            ("client_id", OAUTH_CLIENT_ID),
            ("grant_type", "refresh_token"),
            ("refresh_token", old_refresh),
        ],
    )
    .await?;

    // 401/403 或 error=invalid_grant 都说明授权已被吊销
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(OAuthError::NotAuthorized);
    }
    if !status.is_success() {
        if extract_error_code(&body).as_deref() == Some("invalid_grant") {
            return Err(OAuthError::NotAuthorized);
        }
        return Err(OAuthError::Api(
            extract_error_message(&body).unwrap_or_else(|| format!("HTTP {status}")),
        ));
    }

    // 服务端可能不返回新 refresh_token，此时沿用旧的
    credentials_from_token_body(&body, Some(old_refresh))
}

/// 剩余有效期小于 margin（Mac 版为 5 分钟）即视为即将过期。
pub fn is_expiring_soon(creds: &Credentials, margin_secs: i64) -> bool {
    match creds.expires_at {
        Some(expires_at) => expires_at - now_unix() < margin_secs,
        None => false,
    }
}

/// 从该账号的凭证文件（credentials-<id>.json）读取（不存在返回 Ok(None)）。
///
/// 当前格式：DPAPI（CurrentUser 作用域）加密的二进制密文。
/// 兼容旧版本写出的明文 JSON：DPAPI 解密失败则回退按明文解析，
/// 解析成功立刻以加密形式原地重写（透明迁移）；明文也解析失败按损坏处理返回 None。
pub fn load_credentials(account_id: &str) -> Result<Option<Credentials>, OAuthError> {
    load_from(&credentials_file_path(account_id))
}

/// 原子写入（临时文件 + rename）到该账号的 credentials-<id>.json。
/// 内容：JSON 序列化 → UTF-8 字节 → DPAPI 加密后的二进制密文；
/// DPAPI 失败时宁可报错也不落明文。
pub fn save_credentials(account_id: &str, creds: &Credentials) -> Result<(), OAuthError> {
    save_to(&credentials_file_path(account_id), creds)
}

/// 删除该账号的本地凭证（授权吊销 / 用户退出登录 / 删除账号时调用）。
pub fn clear_credentials(account_id: &str) -> Result<(), OAuthError> {
    remove_file(&credentials_file_path(account_id))
}

/// 旧单账号凭证文件路径（仅迁移用）：{config_dir}/credentials.json
pub(crate) fn legacy_credentials_file_path() -> PathBuf {
    config_dir().join("credentials.json")
}

/// 迁移用：读取旧单账号凭证（含明文原地升级语义，升级写回旧路径）
pub(crate) fn load_legacy_credentials() -> Result<Option<Credentials>, OAuthError> {
    load_from(&legacy_credentials_file_path())
}

/// 迁移用：删除旧单账号凭证文件
pub(crate) fn clear_legacy_credentials() -> Result<(), OAuthError> {
    remove_file(&legacy_credentials_file_path())
}

// ---------------------------------------------------------------------------
// 以下为内部实现（文件读写按路径参数化，账号无关）
// ---------------------------------------------------------------------------

/// 从指定路径读取凭证（语义见 load_credentials 注释）
fn load_from(path: &std::path::Path) -> Result<Option<Credentials>, OAuthError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(OAuthError::Io(e.to_string())),
    };
    // 1) 先按 DPAPI 密文解；解开了但内容非法同样按损坏处理
    if let Ok(json) = dpapi::unprotect(&bytes) {
        return Ok(serde_json::from_slice(&json).ok());
    }
    // 2) 解密失败：回退按明文 JSON 解析（旧版本写出的文件）
    match serde_json::from_slice::<Credentials>(&bytes) {
        Ok(creds) => {
            // 原地升级为密文；重写失败不算致命（下次 load 还会再试）
            let _ = save_to(path, &creds);
            Ok(Some(creds))
        }
        // 3) 明文也解析失败：按无凭证/损坏处理（与 Mac 版 `try?` 解码语义一致）
        Err(_) => Ok(None),
    }
}

/// 原子写入（临时文件 + rename）到指定路径；DPAPI 失败时宁可报错也不落明文
fn save_to(path: &std::path::Path, creds: &Credentials) -> Result<(), OAuthError> {
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| OAuthError::Io(e.to_string()))?;

    let json = serde_json::to_string_pretty(creds).map_err(|e| OAuthError::Io(e.to_string()))?;
    let blob = dpapi::protect(json.as_bytes()).map_err(OAuthError::Io)?;
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, blob).map_err(|e| OAuthError::Io(e.to_string()))?;
    // Windows 上 rename 不允许目标已存在，先删再改名
    if path.exists() {
        std::fs::remove_file(path).map_err(|e| OAuthError::Io(e.to_string()))?;
    }
    std::fs::rename(&tmp_path, path).map_err(|e| OAuthError::Io(e.to_string()))
}

/// 删除指定凭证文件：不存在也算成功
fn remove_file(path: &std::path::Path) -> Result<(), OAuthError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(OAuthError::Io(e.to_string())),
    }
}

// ---------------------------------------------------------------------------
// 以下为内部实现
// ---------------------------------------------------------------------------

/// 设备授权响应（snake_case JSON）
#[derive(Debug, Deserialize)]
struct DeviceAuthResponse {
    user_code: String,
    device_code: String,
    verification_uri: Option<String>,
    verification_uri_complete: Option<String>,
    expires_in: Option<u64>,
    interval: Option<u64>,
}

/// token 端点响应。刷新时服务端可能省略 refresh_token。
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    scope: Option<String>,
    token_type: Option<String>,
}

/// 轮询错误分类（纯逻辑，便于单测）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PollAction {
    /// authorization_pending：按当前间隔继续
    Pending,
    /// slow_down：间隔 +5s
    SlowDown,
    /// expired_token → OAuthError::Expired
    Expired,
    /// access_denied → OAuthError::Denied
    Denied,
    /// 其余错误 → OAuthError::Api
    Api,
}

fn classify_poll_error(code: Option<&str>) -> PollAction {
    match code {
        Some("authorization_pending") => PollAction::Pending,
        Some("slow_down") => PollAction::SlowDown,
        Some("expired_token") => PollAction::Expired,
        Some("access_denied") => PollAction::Denied,
        _ => PollAction::Api,
    }
}

/// 解析 token 响应为 Credentials；fallback_refresh_token 用于刷新时沿用旧 refresh_token。
/// expires_at = now + expires_in。
fn credentials_from_token_body(
    body: &str,
    fallback_refresh_token: Option<&str>,
) -> Result<Credentials, OAuthError> {
    let resp: TokenResponse = serde_json::from_str(body)
        .map_err(|_| OAuthError::Api("授权服务返回了无法解析的响应".into()))?;
    if resp.access_token.is_empty() {
        return Err(OAuthError::Api("授权服务返回了无效的响应".into()));
    }
    Ok(Credentials {
        access_token: resp.access_token,
        refresh_token: resp
            .refresh_token
            .or_else(|| fallback_refresh_token.map(str::to_string)),
        expires_at: resp.expires_in.map(|expires_in| now_unix() + expires_in),
        scope: resp.scope,
        token_type: resp.token_type,
    })
}

/// 按优先级抽取错误消息：error_description > message > detail > error > 原文
fn extract_error_message(body: &str) -> Option<String> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        for key in ["error_description", "message", "detail", "error"] {
            if let Some(s) = value.get(key).and_then(|v| v.as_str()) {
                return Some(s.to_string());
            }
        }
    }
    let text = body.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// 抽取 error 字段（用于错误分类）
fn extract_error_code(body: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
    value.get("error")?.as_str().map(str::to_string)
}

/// 拼接 application/x-www-form-urlencoded body（key/value 均 percent-encode）
fn form_body(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// 构造带 UA / 30s 超时的 HTTP 客户端
fn http_client() -> Result<reqwest::Client, OAuthError> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|e| OAuthError::Http(e.to_string()))
}

/// 验证地址白名单校验（纯函数）：必须 https scheme 且 host 为 kimi.com 或其子域。
/// 先剥 userinfo（防 https://kimi.com@evil.com 伪装）再去端口，按小写 host 比较
fn is_trusted_verification_uri(uri: &str) -> bool {
    let Some(rest) = uri.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    let host = host_port
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    host == "kimi.com" || host.ends_with(".kimi.com")
}

/// POST 表单（带 X-Msh-* 设备身份头），返回 (状态码, 响应文本)
async fn post_form(
    client: &reqwest::Client,
    url: &str,
    params: &[(&str, &str)],
) -> Result<(reqwest::StatusCode, String), OAuthError> {
    let mut request = client
        .post(url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .body(form_body(params));
    for (name, value) in identity_headers() {
        request = request.header(name, value);
    }
    let response = request
        .send()
        .await
        .map_err(|e| OAuthError::Http(e.to_string()))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| OAuthError::Http(e.to_string()))?;
    Ok((status, body))
}

/// 与 CLI 对齐的 X-Msh-* 设备身份头
fn identity_headers() -> Vec<(&'static str, String)> {
    let mut headers = vec![
        ("X-Msh-Platform", "kimi_code_cli".to_string()),
        ("X-Msh-Version", env!("CARGO_PKG_VERSION").to_string()),
        ("X-Msh-Device-Name", device_name()),
        ("X-Msh-Device-Model", device_model()),
        ("X-Msh-Os-Version", os_version_string()),
    ];
    if let Some(device_id) = load_or_create_device_id() {
        headers.push(("X-Msh-Device-Id", device_id));
    }
    headers
}

/// 读取 %USERPROFILE%\.kimi-code\device_id；不存在则生成 UUID v4 写入（与 CLI 共享）
fn load_or_create_device_id() -> Option<String> {
    let path = device_id_path()?;
    if let Ok(text) = std::fs::read_to_string(&path) {
        let id = text.trim();
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok()?;
    }
    std::fs::write(&path, &id).ok()?;
    Some(id)
}

fn device_id_path() -> Option<PathBuf> {
    user_home_dir().map(|home| home.join(".kimi-code").join("device_id"))
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// 主机名（Windows 上即 COMPUTERNAME）
fn device_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows".to_string())
}

/// 设备型号，如 "Windows 11"（build >= 22000）
fn device_model() -> String {
    match windows_os_version() {
        Some((major, _, build)) if major >= 10 && build >= 22000 => "Windows 11".to_string(),
        Some((major, _, _)) if major >= 10 => "Windows 10".to_string(),
        Some(_) => "Windows".to_string(),
        None => "Windows".to_string(),
    }
}

/// 尽量取真实系统版本，取不到给合理默认
fn os_version_string() -> String {
    match windows_os_version() {
        Some((major, minor, build)) => format!("{major}.{minor}.{build}"),
        None => "10.0".to_string(),
    }
}

/// 通过 ntdll!RtlGetVersion 取真实版本（不受 manifest 影响），无需额外依赖
#[cfg(windows)]
fn windows_os_version() -> Option<(u32, u32, u32)> {
    #[repr(C)]
    struct OsVersionInfoW {
        os_version_info_size: u32,
        major_version: u32,
        minor_version: u32,
        build_number: u32,
        platform_id: u32,
        csd_version: [u16; 128],
    }

    #[link(name = "ntdll")]
    extern "system" {
        fn RtlGetVersion(info: *mut OsVersionInfoW) -> i32;
    }

    let mut info = OsVersionInfoW {
        os_version_info_size: std::mem::size_of::<OsVersionInfoW>() as u32,
        major_version: 0,
        minor_version: 0,
        build_number: 0,
        platform_id: 0,
        csd_version: [0; 128],
    };
    // NTSTATUS 0 = STATUS_SUCCESS
    if unsafe { RtlGetVersion(&mut info) } == 0 {
        Some((info.major_version, info.minor_version, info.build_number))
    } else {
        None
    }
}

#[cfg(not(windows))]
fn windows_os_version() -> Option<(u32, u32, u32)> {
    None
}

/// 凭证文件目录：KIMICODEBAR_CONFIG_DIR 覆盖（测试/便携模式），否则 %APPDATA%\KimiCodeBar
fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("KIMICODEBAR_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return PathBuf::from(appdata).join("KimiCodeBar");
    }
    // 兜底：用户目录下 AppData\Roaming，再不行用临时目录
    if let Some(home) = user_home_dir() {
        return home.join("AppData").join("Roaming").join("KimiCodeBar");
    }
    std::env::temp_dir().join("KimiCodeBar")
}

/// 该账号的凭证文件路径：{config_dir}/credentials-<id>.json
fn credentials_file_path(account_id: &str) -> PathBuf {
    config_dir().join(format!("credentials-{account_id}.json"))
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 错误分类 ----

    #[test]
    fn classify_poll_error_maps_known_codes() {
        assert_eq!(
            classify_poll_error(Some("authorization_pending")),
            PollAction::Pending
        );
        assert_eq!(classify_poll_error(Some("slow_down")), PollAction::SlowDown);
        assert_eq!(
            classify_poll_error(Some("expired_token")),
            PollAction::Expired
        );
        assert_eq!(
            classify_poll_error(Some("access_denied")),
            PollAction::Denied
        );
    }

    #[test]
    fn classify_poll_error_unknown_falls_back_to_api() {
        assert_eq!(classify_poll_error(Some("invalid_grant")), PollAction::Api);
        assert_eq!(classify_poll_error(Some("server_error")), PollAction::Api);
        assert_eq!(classify_poll_error(None), PollAction::Api);
    }

    // ---- is_expiring_soon ----

    #[test]
    fn expiring_soon_within_margin() {
        let creds = Credentials {
            access_token: "t".into(),
            refresh_token: None,
            expires_at: Some(now_unix() + 100),
            scope: None,
            token_type: None,
        };
        assert!(is_expiring_soon(&creds, 300));
        assert!(!is_expiring_soon(&creds, 50));
    }

    #[test]
    fn expiring_soon_past_expiry_is_expiring() {
        let creds = Credentials {
            access_token: "t".into(),
            refresh_token: None,
            expires_at: Some(now_unix() - 10),
            scope: None,
            token_type: None,
        };
        assert!(is_expiring_soon(&creds, 300));
    }

    #[test]
    fn expiring_soon_without_expires_at_is_false() {
        let creds = Credentials {
            access_token: "t".into(),
            refresh_token: None,
            expires_at: None,
            scope: None,
            token_type: None,
        };
        assert!(!is_expiring_soon(&creds, 300));
    }

    // ---- 凭证 save/load/clear 往返 ----

    // 环境变量是进程级全局状态，凡改动 KIMICODEBAR_CONFIG_DIR 的测试都须持锁串行；
    // 锁为全库共享（lib.rs::TEST_ENV_LOCK），与 storage 等模块的同类测试互斥
    use crate::TEST_ENV_LOCK as ENV_LOCK;

    #[test]
    fn credentials_save_load_clear_roundtrip() {
        let _guard = ENV_LOCK.lock().unwrap();
        // KIMICODEBAR_CONFIG_DIR 指向独立临时目录，避免碰真实 %APPDATA%
        let dir = std::env::temp_dir().join(format!("kimicodebar-test-{}", uuid::Uuid::new_v4()));
        std::env::set_var("KIMICODEBAR_CONFIG_DIR", &dir);

        // 未保存时读取为 None
        assert!(load_credentials("acc1").unwrap().is_none());

        let creds = Credentials {
            access_token: "access-123".into(),
            refresh_token: Some("refresh-456".into()),
            expires_at: Some(1_900_000_000),
            scope: Some("scope".into()),
            token_type: Some("Bearer".into()),
        };
        save_credentials("acc1", &creds).unwrap();
        assert!(dir.join("credentials-acc1.json").exists());
        // 临时文件不应残留
        assert!(!dir.join("credentials-acc1.json.tmp").exists());

        let loaded = load_credentials("acc1").unwrap().expect("应能读回凭证");
        assert_eq!(loaded.access_token, "access-123");
        assert_eq!(loaded.refresh_token.as_deref(), Some("refresh-456"));
        assert_eq!(loaded.expires_at, Some(1_900_000_000));
        assert_eq!(loaded.scope.as_deref(), Some("scope"));
        assert_eq!(loaded.token_type.as_deref(), Some("Bearer"));

        // 另一个账号不受污染
        assert!(load_credentials("acc2").unwrap().is_none());

        // 覆盖写入（rename 目标已存在的路径）
        let updated = Credentials {
            access_token: "access-789".into(),
            ..creds.clone()
        };
        save_credentials("acc1", &updated).unwrap();
        assert_eq!(
            load_credentials("acc1").unwrap().unwrap().access_token,
            "access-789"
        );

        // 磁盘格式为 DPAPI 密文：不含任何明文 token / JSON 键名（Windows 上）
        let raw = std::fs::read(dir.join("credentials-acc1.json")).unwrap();
        #[cfg(windows)]
        {
            let raw_text = String::from_utf8_lossy(&raw);
            assert!(!raw_text.contains("access-789"));
            assert!(!raw_text.contains("refresh-456"));
            assert!(!raw_text.contains("access_token"));
            // 密文整体也不应能按 JSON 解析
            assert!(serde_json::from_slice::<serde_json::Value>(&raw).is_err());
        }
        #[cfg(not(windows))]
        {
            // 非 Windows 下 DPAPI 为透传实现，磁盘仍是 snake_case JSON
            let raw_text = String::from_utf8(raw).unwrap();
            assert!(raw_text.contains("\"access_token\""));
            assert!(raw_text.contains("\"refresh_token\""));
            assert!(raw_text.contains("\"expires_at\""));
        }

        clear_credentials("acc1").unwrap();
        assert!(load_credentials("acc1").unwrap().is_none());
        // 重复删除也应成功
        clear_credentials("acc1").unwrap();

        std::env::remove_var("KIMICODEBAR_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_credentials_corrupt_file_returns_none() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("kimicodebar-test-{}", uuid::Uuid::new_v4()));
        std::env::set_var("KIMICODEBAR_CONFIG_DIR", &dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("credentials-acc1.json"), "not json").unwrap();

        assert!(load_credentials("acc1").unwrap().is_none());

        // 随机二进制（既非 DPAPI 密文也非明文 JSON）同样按损坏处理
        let garbage: Vec<u8> = (0u8..=255).collect();
        std::fs::write(dir.join("credentials-acc1.json"), garbage).unwrap();
        assert!(load_credentials("acc1").unwrap().is_none());

        std::env::remove_var("KIMICODEBAR_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_credentials_migrates_plaintext_file_to_encrypted() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("kimicodebar-test-{}", uuid::Uuid::new_v4()));
        std::env::set_var("KIMICODEBAR_CONFIG_DIR", &dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 旧版本写出的明文 snake_case JSON
        let plaintext = r#"{
  "access_token": "legacy-access",
  "refresh_token": "legacy-refresh",
  "expires_at": 1900000000,
  "scope": "legacy-scope",
  "token_type": "Bearer"
}"#;
        std::fs::write(dir.join("credentials-acc1.json"), plaintext).unwrap();

        // 明文旧文件可正常读出
        let creds = load_credentials("acc1")
            .unwrap()
            .expect("明文旧文件应能读出");
        assert_eq!(creds.access_token, "legacy-access");
        assert_eq!(creds.refresh_token.as_deref(), Some("legacy-refresh"));
        assert_eq!(creds.expires_at, Some(1_900_000_000));
        assert_eq!(creds.scope.as_deref(), Some("legacy-scope"));

        // 读取后文件已被原地升级为密文
        let raw = std::fs::read(dir.join("credentials-acc1.json")).unwrap();
        let raw_text = String::from_utf8_lossy(&raw);
        assert!(
            !raw_text.contains("legacy-access"),
            "迁移后文件不应再含明文 token"
        );
        #[cfg(windows)]
        assert!(serde_json::from_slice::<serde_json::Value>(&raw).is_err());

        // 升级后的密文仍可正常读回
        let creds = load_credentials("acc1")
            .unwrap()
            .expect("迁移后的密文应能读回");
        assert_eq!(creds.access_token, "legacy-access");
        assert_eq!(creds.refresh_token.as_deref(), Some("legacy-refresh"));
        assert_eq!(creds.expires_at, Some(1_900_000_000));

        std::env::remove_var("KIMICODEBAR_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- 验证地址白名单校验 ----

    #[test]
    fn verification_uri_trust_check() {
        // 放行：kimi.com 及其子域的 https 地址（含路径 / query / 端口）
        for good in [
            "https://kimi.com/device",
            "https://auth.kimi.com/api/oauth/device?user_code=ABCD",
            "https://www.kimi.com:443/x",
        ] {
            assert!(is_trusted_verification_uri(good), "应放行: {good}");
        }
        // 拒绝：非 https、异 host、后缀伪装、userinfo 伪装、危险 scheme
        for bad in [
            "",
            "http://kimi.com/device",
            "https://evil.com",
            "https://kimi.com.evil.com",
            "https://evil-kimi.com",
            "https://kimi.com@evil.com/",
            "javascript:alert(1)",
            "file:///C:/Windows/System32/cmd.exe",
        ] {
            assert!(!is_trusted_verification_uri(bad), "应拒绝: {bad}");
        }
    }

    // ---- form body 构造 ----

    #[test]
    fn form_body_encodes_pairs() {
        let body = form_body(&[
            ("client_id", OAUTH_CLIENT_ID),
            ("grant_type", DEVICE_GRANT_TYPE),
            ("device_code", "dev/code=1"),
        ]);
        assert_eq!(
            body,
            format!(
                "client_id={OAUTH_CLIENT_ID}&grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code&device_code=dev%2Fcode%3D1"
            )
        );
    }

    #[test]
    fn form_body_device_authorization_only_client_id() {
        // start_device_auth 的 body 只能有 client_id
        let body = form_body(&[("client_id", OAUTH_CLIENT_ID)]);
        assert_eq!(body, format!("client_id={OAUTH_CLIENT_ID}"));
    }

    // ---- token 响应解析 ----

    #[test]
    fn token_body_parses_and_computes_expires_at() {
        let before = now_unix();
        let body = r#"{"access_token":"at","refresh_token":"rt","expires_in":3600,"scope":"s","token_type":"Bearer"}"#;
        let creds = credentials_from_token_body(body, None).unwrap();
        assert_eq!(creds.access_token, "at");
        assert_eq!(creds.refresh_token.as_deref(), Some("rt"));
        let expires_at = creds.expires_at.unwrap();
        assert!(expires_at >= before + 3600 && expires_at <= now_unix() + 3600);
        assert_eq!(creds.scope.as_deref(), Some("s"));
        assert_eq!(creds.token_type.as_deref(), Some("Bearer"));
    }

    #[test]
    fn token_body_keeps_old_refresh_token_when_missing() {
        let body = r#"{"access_token":"at2","expires_in":60}"#;
        let creds = credentials_from_token_body(body, Some("old-rt")).unwrap();
        assert_eq!(creds.refresh_token.as_deref(), Some("old-rt"));
    }

    #[test]
    fn token_body_rejects_empty_access_token() {
        let body = r#"{"access_token":"","expires_in":60}"#;
        assert!(credentials_from_token_body(body, None).is_err());
        assert!(credentials_from_token_body("not json", None).is_err());
    }

    // ---- 错误消息抽取 ----

    #[test]
    fn extract_error_message_priority() {
        assert_eq!(
            extract_error_message(r#"{"error":"access_denied","error_description":"用户拒绝"}"#),
            Some("用户拒绝".to_string())
        );
        assert_eq!(
            extract_error_message(r#"{"message":"m","detail":"d","error":"e"}"#),
            Some("m".to_string())
        );
        assert_eq!(
            extract_error_message(r#"{"error":"access_denied"}"#),
            Some("access_denied".to_string())
        );
        assert_eq!(
            extract_error_message("plain text"),
            Some("plain text".to_string())
        );
        assert_eq!(extract_error_message(""), None);
    }

    #[test]
    fn extract_error_code_reads_error_field() {
        assert_eq!(
            extract_error_code(r#"{"error":"slow_down"}"#).as_deref(),
            Some("slow_down")
        );
        assert_eq!(extract_error_code(r#"{"message":"x"}"#), None);
        assert_eq!(extract_error_code("not json"), None);
    }
}
