//! 凭证管理：API Key 存 Windows 凭据管理器（keyring），OAuth 走 kimi::oauth。
//!
//! `get_active_token` 是数据链路取 token 的唯一入口：按设置里的登录方式选择，
//! 未显式选择时优先 API Key、其次 OAuth；OAuth token 临期自动刷新。

use keyring::Entry;
use thiserror::Error;

use crate::kimi::oauth::{self, OAuthError};
use crate::storage;

/// keyring 服务名（Windows 凭据管理器里的"服务/目标"）
const KEYRING_SERVICE: &str = "KimiCodeBar";
/// keyring 条目名（凭据管理器里的"用户名"）
const KEYRING_USER: &str = "api_key";
/// keyring 条目名：网页端 kimi-auth token（月度总量用）
const KEYRING_WEB_USER: &str = "web_token";
/// OAuth token 剩余有效期小于该值（秒）即提前刷新，与 Mac 版 5 分钟一致
const REFRESH_MARGIN_SECS: i64 = 300;

#[derive(Debug, Error)]
pub enum CredError {
    #[error("凭据管理器错误: {0}")]
    Keyring(String),
    #[error("OAuth 错误: {0}")]
    OAuth(#[from] OAuthError),
    #[error("本地存储错误: {0}")]
    Storage(String),
}

/// 当前生效的凭证类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialKind {
    ApiKey,
    OAuth,
}

/// 保存 API Key 到 Windows 凭据管理器
pub fn save_api_key(key: &str) -> Result<(), CredError> {
    entry()?
        .set_password(key)
        .map_err(|e| CredError::Keyring(e.to_string()))
}

/// 读取 API Key：未保存过 → Ok(None)
pub fn load_api_key() -> Result<Option<String>, CredError> {
    match entry()?.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(CredError::Keyring(e.to_string())),
    }
}

/// 删除 API Key：本来就不存在也算成功
pub fn clear_api_key() -> Result<(), CredError> {
    match entry()?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(CredError::Keyring(e.to_string())),
    }
}

/// 保存网页端 kimi-auth token（月度总量用）到 Windows 凭据管理器
pub fn save_web_token(token: &str) -> Result<(), CredError> {
    web_entry()?
        .set_password(token)
        .map_err(|e| CredError::Keyring(e.to_string()))
}

/// 读取网页端 token：未保存过 → Ok(None)
pub fn load_web_token() -> Result<Option<String>, CredError> {
    match web_entry()?.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(CredError::Keyring(e.to_string())),
    }
}

/// 删除网页端 token：本来就不存在也算成功
pub fn clear_web_token() -> Result<(), CredError> {
    match web_entry()?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(CredError::Keyring(e.to_string())),
    }
}

/// 取当前生效的 token：
/// - `settings.login_method == "api_key"` → 只查凭据管理器
/// - `settings.login_method == "oauth"` → 只查 OAuth 凭证
/// - 未显式选择 → 优先 API Key，其次 OAuth
///
/// OAuth token 临期（<300s）时自动刷新并回写；刷新返回 NotAuthorized（授权已吊销）
/// 时清除本地凭证并返回 Ok(None)，由上层 UI 提示重新登录。
pub async fn get_active_token() -> Result<Option<(CredentialKind, String)>, CredError> {
    let settings = storage::load_settings().map_err(CredError::Storage)?;
    match settings.login_method.as_deref() {
        Some("api_key") => Ok(load_api_key()?.map(|key| (CredentialKind::ApiKey, key))),
        Some("oauth") => oauth_token().await,
        // 未显式选择（或值非法）：优先 api_key，其次 oauth
        _ => {
            if let Some(key) = load_api_key()? {
                return Ok(Some((CredentialKind::ApiKey, key)));
            }
            oauth_token().await
        }
    }
}

// ---------------------------------------------------------------------------
// 以下为内部实现
// ---------------------------------------------------------------------------

/// OAuth 路径：读本地凭证 → 临期刷新 → 返回 access_token
async fn oauth_token() -> Result<Option<(CredentialKind, String)>, CredError> {
    let Some(creds) = oauth::load_credentials()? else {
        return Ok(None);
    };

    let creds = if oauth::is_expiring_soon(&creds, REFRESH_MARGIN_SECS) {
        match oauth::refresh_token(&creds).await {
            Ok(new_creds) => {
                tracing::info!("OAuth token 刷新成功");
                oauth::save_credentials(&new_creds)?;
                new_creds
            }
            Err(OAuthError::NotAuthorized) => {
                // 授权已被吊销：清掉本地凭证，让上层提示重新登录
                tracing::warn!("OAuth 授权已被吊销，已清除本地凭证");
                oauth::clear_credentials()?;
                return Ok(None);
            }
            // 其余刷新失败（网络抖动等）：token 未必真的失效，先继续用旧的，
            // 真失效时 usages 接口会返回 401，由上层按"凭证无效"提示
            Err(e) => {
                tracing::warn!("OAuth token 刷新失败，暂用旧 token: {e}");
                creds
            }
        }
    } else {
        creds
    };

    Ok(Some((CredentialKind::OAuth, creds.access_token)))
}

fn entry() -> Result<Entry, CredError> {
    Entry::new(KEYRING_SERVICE, KEYRING_USER).map_err(|e| CredError::Keyring(e.to_string()))
}

/// 网页端 token 的 keyring 条目（与 api_key 同 service，不同 key）
fn web_entry() -> Result<Entry, CredError> {
    Entry::new(KEYRING_SERVICE, KEYRING_WEB_USER).map_err(|e| CredError::Keyring(e.to_string()))
}

#[cfg(test)]
mod tests {
    /// 给架构方手工种 Key 用：
    /// `set KIMI_API_KEY=sk-xxx && cargo test --offline -- --ignored seed_api_key --nocapture`
    /// 写入真实 Windows 凭据管理器（service "KimiCodeBar" / key "api_key"），断言写后能读回。
    #[test]
    #[ignore]
    fn seed_api_key() {
        let key = std::env::var("KIMI_API_KEY").expect("请先设置 KIMI_API_KEY 环境变量");
        super::save_api_key(&key).expect("写入凭据管理器失败");
        let loaded = super::load_api_key().expect("读取凭据管理器失败");
        assert_eq!(loaded.as_deref(), Some(key.as_str()));
        println!("API Key 已写入 Windows 凭据管理器（KimiCodeBar/api_key）");
    }
}
