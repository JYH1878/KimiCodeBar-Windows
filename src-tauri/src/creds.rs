//! 凭证管理：API Key / 网页 token 存 Windows 凭据管理器（keyring），OAuth 走 kimi::oauth。
//!
//! 多账号：keyring 槽位名带账号 id（形如 `api_key/<id>`），service 恒为 KimiCodeBar
//! （测试可用 `KIMICODEBAR_KEYRING_SERVICE` 环境变量覆盖 service 名，与真实凭据管理器隔离）。
//! 旧单账号槽位（裸 `api_key` / `web_token` / `web_refresh_token`）仅迁移模块
//! （crate::migrate）读取，确认新槽位能读回后才删除。
//!
//! `get_active_token` 是数据链路取 token 的唯一入口：按账号的登录方式选择，
//! 未显式选择时优先 API Key、其次 OAuth；OAuth token 临期自动刷新。

use keyring::Entry;
use thiserror::Error;

use crate::kimi::oauth::{self, OAuthError};
use crate::storage::Account;

/// keyring 服务名（Windows 凭据管理器里的"服务/目标"）
const KEYRING_SERVICE: &str = "KimiCodeBar";
/// 测试隔离用环境变量：覆盖 keyring service 名（避免测试碰真实凭据）
const KEYRING_SERVICE_ENV: &str = "KIMICODEBAR_KEYRING_SERVICE";
/// 槽位名前缀：API Key（完整槽位名 `api_key/<账号id>`）
pub(crate) const SLOT_API_KEY: &str = "api_key";
/// 槽位名前缀：网页端 kimi-auth token（月度总量用，旧鉴权体系；仍保留以便过渡期兼容）
pub(crate) const SLOT_WEB_TOKEN: &str = "web_token";
/// 槽位名前缀：网页端 refresh_token（月度总量用，新鉴权体系）
pub(crate) const SLOT_WEB_REFRESH_TOKEN: &str = "web_refresh_token";
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

/// 保存该账号的 API Key 到 Windows 凭据管理器
pub fn save_api_key(account_id: &str, key: &str) -> Result<(), CredError> {
    save(SLOT_API_KEY, account_id, key)
}

/// 读取该账号的 API Key：未保存过 → Ok(None)
pub fn load_api_key(account_id: &str) -> Result<Option<String>, CredError> {
    load(SLOT_API_KEY, account_id)
}

/// 删除该账号的 API Key：本来就不存在也算成功
pub fn clear_api_key(account_id: &str) -> Result<(), CredError> {
    clear(SLOT_API_KEY, account_id)
}

/// 保存该账号的网页端 kimi-auth token（月度总量用）到 Windows 凭据管理器
pub fn save_web_token(account_id: &str, token: &str) -> Result<(), CredError> {
    save(SLOT_WEB_TOKEN, account_id, token)
}

/// 读取该账号的网页端 token：未保存过 → Ok(None)
pub fn load_web_token(account_id: &str) -> Result<Option<String>, CredError> {
    load(SLOT_WEB_TOKEN, account_id)
}

/// 删除该账号的网页端 token：本来就不存在也算成功
pub fn clear_web_token(account_id: &str) -> Result<(), CredError> {
    clear(SLOT_WEB_TOKEN, account_id)
}

/// 保存该账号的网页端 refresh_token（月度总量用，新鉴权体系）到 Windows 凭据管理器。
/// 新体系每次续期轮换 refresh_token，新的必须落盘，丢旧即失效。
pub fn save_web_refresh_token(account_id: &str, token: &str) -> Result<(), CredError> {
    save(SLOT_WEB_REFRESH_TOKEN, account_id, token)
}

/// 读取该账号的网页端 refresh_token：未保存过 → Ok(None)
pub fn load_web_refresh_token(account_id: &str) -> Result<Option<String>, CredError> {
    load(SLOT_WEB_REFRESH_TOKEN, account_id)
}

/// 删除该账号的网页端 refresh_token：本来就不存在也算成功
pub fn clear_web_refresh_token(account_id: &str) -> Result<(), CredError> {
    clear(SLOT_WEB_REFRESH_TOKEN, account_id)
}

/// 取该账号当前生效的 token：
/// - `account.login_method == "api_key"` → 只查凭据管理器
/// - `account.login_method == "oauth"` → 只查 OAuth 凭证
/// - 未显式选择 → 优先 API Key，其次 OAuth
///
/// OAuth token 临期（<300s）时自动刷新并回写；刷新返回 NotAuthorized（授权已吊销）
/// 时清除本地凭证并返回 Ok(None)，由上层 UI 提示重新登录。
pub async fn get_active_token(
    account: &Account,
) -> Result<Option<(CredentialKind, String)>, CredError> {
    match account.login_method.as_deref() {
        Some("api_key") => Ok(load_api_key(&account.id)?.map(|key| (CredentialKind::ApiKey, key))),
        Some("oauth") => oauth_token(&account.id).await,
        // 未显式选择（或值非法）：优先 api_key，其次 oauth
        _ => {
            if let Some(key) = load_api_key(&account.id)? {
                return Ok(Some((CredentialKind::ApiKey, key)));
            }
            oauth_token(&account.id).await
        }
    }
}

// ---------------------------------------------------------------------------
// 旧单账号槽位（裸槽位名，不带账号 id）：仅迁移模块使用
// ---------------------------------------------------------------------------

/// 读取旧版单账号槽位（如裸 `api_key`）：未保存过 → Ok(None)
pub(crate) fn legacy_load(slot_base: &str) -> Result<Option<String>, CredError> {
    match legacy_entry(slot_base)?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(CredError::Keyring(e.to_string())),
    }
}

/// 删除旧版单账号槽位：本来就不存在也算成功
pub(crate) fn legacy_delete(slot_base: &str) -> Result<(), CredError> {
    match legacy_entry(slot_base)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(CredError::Keyring(e.to_string())),
    }
}

// ---------------------------------------------------------------------------
// 以下为内部实现
// ---------------------------------------------------------------------------

/// OAuth 路径：读本地凭证 → 临期刷新 → 返回 access_token
async fn oauth_token(account_id: &str) -> Result<Option<(CredentialKind, String)>, CredError> {
    let Some(creds) = oauth::load_credentials(account_id)? else {
        return Ok(None);
    };

    let creds = if oauth::is_expiring_soon(&creds, REFRESH_MARGIN_SECS) {
        match oauth::refresh_token(&creds).await {
            Ok(new_creds) => {
                tracing::info!("OAuth token 刷新成功");
                oauth::save_credentials(account_id, &new_creds)?;
                new_creds
            }
            Err(OAuthError::NotAuthorized) => {
                // 授权已被吊销：清掉本地凭证，让上层提示重新登录
                tracing::warn!("OAuth 授权已被吊销，已清除本地凭证");
                oauth::clear_credentials(account_id)?;
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

/// keyring service 名：测试环境变量覆盖，否则固定 KimiCodeBar
fn service() -> String {
    std::env::var(KEYRING_SERVICE_ENV).unwrap_or_else(|_| KEYRING_SERVICE.to_string())
}

/// 该账号某类凭证的 keyring 条目（槽位名 `<前缀>/<账号id>`）
fn entry(slot_base: &str, account_id: &str) -> Result<Entry, CredError> {
    Entry::new(&service(), &format!("{slot_base}/{account_id}"))
        .map_err(|e| CredError::Keyring(e.to_string()))
}

/// 旧版单账号槽位的 keyring 条目（裸槽位名）
fn legacy_entry(slot_base: &str) -> Result<Entry, CredError> {
    Entry::new(&service(), slot_base).map_err(|e| CredError::Keyring(e.to_string()))
}

fn save(slot_base: &str, account_id: &str, value: &str) -> Result<(), CredError> {
    entry(slot_base, account_id)?
        .set_password(value)
        .map_err(|e| CredError::Keyring(e.to_string()))
}

fn load(slot_base: &str, account_id: &str) -> Result<Option<String>, CredError> {
    match entry(slot_base, account_id)?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(CredError::Keyring(e.to_string())),
    }
}

fn clear(slot_base: &str, account_id: &str) -> Result<(), CredError> {
    match entry(slot_base, account_id)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(CredError::Keyring(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    // 环境变量是进程级全局状态，凡改动 KIMICODEBAR_KEYRING_SERVICE 的测试都须持锁串行；
    // 锁为全库共享（lib.rs::TEST_ENV_LOCK），与 storage 等模块的同类测试互斥
    use crate::TEST_ENV_LOCK as ENV_LOCK;

    /// 给架构方手工种 Key 用（写入真实 Windows 凭据管理器）：
    /// `set KIMI_API_KEY=sk-xxx && set KIMICODEBAR_TEST_ACCOUNT_ID=<账号id> && cargo test --offline -- --ignored seed_api_key --nocapture`
    #[test]
    #[ignore]
    fn seed_api_key() {
        let key = std::env::var("KIMI_API_KEY").expect("请先设置 KIMI_API_KEY 环境变量");
        let account_id = std::env::var("KIMICODEBAR_TEST_ACCOUNT_ID")
            .expect("请先设置 KIMICODEBAR_TEST_ACCOUNT_ID 环境变量（目标账号 id）");
        super::save_api_key(&account_id, &key).expect("写入凭据管理器失败");
        let loaded = super::load_api_key(&account_id).expect("读取凭据管理器失败");
        assert_eq!(loaded.as_deref(), Some(key.as_str()));
        println!("API Key 已写入 Windows 凭据管理器（KimiCodeBar/api_key/{account_id}）");
    }

    /// 独立 keyring service 名，避免碰真实凭据管理器里的生产条目
    fn use_test_service() -> String {
        let service = format!("KimiCodeBar-test-{}", uuid::Uuid::new_v4());
        std::env::set_var(super::KEYRING_SERVICE_ENV, &service);
        service
    }

    fn cleanup_service() {
        std::env::remove_var(super::KEYRING_SERVICE_ENV);
    }

    #[test]
    fn slots_isolated_per_account() {
        let _guard = ENV_LOCK.lock().unwrap();
        use_test_service();

        // 两个账号三类凭证各自独立
        super::save_api_key("acc-a", "key-a").unwrap();
        super::save_api_key("acc-b", "key-b").unwrap();
        super::save_web_token("acc-a", "web-a").unwrap();
        super::save_web_refresh_token("acc-b", "refresh-b").unwrap();

        assert_eq!(
            super::load_api_key("acc-a").unwrap().as_deref(),
            Some("key-a")
        );
        assert_eq!(
            super::load_api_key("acc-b").unwrap().as_deref(),
            Some("key-b")
        );
        assert_eq!(
            super::load_web_token("acc-a").unwrap().as_deref(),
            Some("web-a")
        );
        assert!(super::load_web_token("acc-b").unwrap().is_none());
        assert!(super::load_web_refresh_token("acc-a").unwrap().is_none());
        assert_eq!(
            super::load_web_refresh_token("acc-b").unwrap().as_deref(),
            Some("refresh-b")
        );

        // 清 acc-a 不影响 acc-b
        super::clear_api_key("acc-a").unwrap();
        assert!(super::load_api_key("acc-a").unwrap().is_none());
        assert_eq!(
            super::load_api_key("acc-b").unwrap().as_deref(),
            Some("key-b")
        );
        // 重复删除也算成功
        super::clear_api_key("acc-a").unwrap();

        // 收尾：清空测试槽位
        super::clear_api_key("acc-b").unwrap();
        super::clear_web_token("acc-a").unwrap();
        super::clear_web_refresh_token("acc-b").unwrap();
        cleanup_service();
    }

    #[test]
    fn legacy_slots_distinct_from_account_slots() {
        let _guard = ENV_LOCK.lock().unwrap();
        use_test_service();

        // 旧版裸槽位与新版带 id 槽位互不影响（迁移正确性的前提）
        super::legacy_delete(super::SLOT_API_KEY).unwrap();
        super::clear_api_key("acc-a").unwrap();
        assert!(super::legacy_load(super::SLOT_API_KEY).unwrap().is_none());
        assert!(super::load_api_key("acc-a").unwrap().is_none());

        cleanup_service();
    }
}
