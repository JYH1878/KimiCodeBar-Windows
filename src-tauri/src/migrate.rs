//! 旧单账号数据 → 多账号的自动迁移。
//!
//! 触发时机：main() 第一行（GUI 与 CLI --status 都会经过），幂等——
//! settings.json 已含 `accounts` 键（哪怕是空数组）即视为已迁移，直接跳过。
//!
//! 迁移顺序（绝不丢凭证）：
//! 1. 读出全部旧数据（keyring 三裸槽位 / credentials.json / cache.json /
//!    history.json / settings.login_method）；一样都没有 → 新装，不动任何文件；
//! 2. 建「账号 1」（uuid id，login_method 继承旧全局值）；
//! 3. 逐件写入新位置并读回校验（新 keyring 槽位 `xxx/<id>` / credentials-<id>.json /
//!    cache-<id>.json / history-<id>.json）——此阶段不删任何旧数据；
//! 4. settings.json 落盘（accounts=[账号1]，旧全局 login_method 清空）；
//! 5. 最后才删旧槽位/旧文件（删除失败只记日志——数据已在新位置，不影响使用）。
//!
//! 任一步失败：整体中止（旧数据原样保留），下次启动重试。

use crate::creds;
use crate::history;
use crate::kimi::oauth;
use crate::storage::{self, Account};

/// 执行迁移；Ok(true) = 本次做了迁移，Ok(false) = 无需迁移（已迁移过或全新安装）
pub fn migrate_legacy_to_accounts() -> Result<bool, String> {
    // settings.json 已含 accounts 键 → 已迁移过，跳过
    if settings_json_has_accounts_key()? {
        return Ok(false);
    }

    let legacy = read_legacy()?;
    if legacy.is_empty() {
        // 全新安装：没有可迁移的数据
        return Ok(false);
    }

    // 建「账号 1」：继承旧全局登录方式
    let account = Account {
        id: uuid::Uuid::new_v4().to_string(),
        name: "账号 1".to_string(),
        login_method: legacy.login_method.clone(),
    };
    tracing::info!("检测到旧单账号数据，迁移为「账号 1」（id={}）", account.id);

    // ---- 阶段 1：逐件写入新位置并读回校验（不删旧数据） ----
    if let Some(key) = &legacy.api_key {
        creds::save_api_key(&account.id, key).map_err(|e| e.to_string())?;
        if creds::load_api_key(&account.id)
            .map_err(|e| e.to_string())?
            .as_deref()
            != Some(key)
        {
            return Err("迁移 api_key 后读回校验失败".to_string());
        }
    }
    if let Some(token) = &legacy.web_token {
        creds::save_web_token(&account.id, token).map_err(|e| e.to_string())?;
        if creds::load_web_token(&account.id)
            .map_err(|e| e.to_string())?
            .as_deref()
            != Some(token)
        {
            return Err("迁移 web_token 后读回校验失败".to_string());
        }
    }
    if let Some(token) = &legacy.web_refresh_token {
        creds::save_web_refresh_token(&account.id, token).map_err(|e| e.to_string())?;
        if creds::load_web_refresh_token(&account.id)
            .map_err(|e| e.to_string())?
            .as_deref()
            != Some(token)
        {
            return Err("迁移 web_refresh_token 后读回校验失败".to_string());
        }
    }
    if let Some(oauth_creds) = &legacy.oauth {
        oauth::save_credentials(&account.id, oauth_creds).map_err(|e| e.to_string())?;
        let back = oauth::load_credentials(&account.id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "迁移 OAuth 凭证后读回为空".to_string())?;
        if back.access_token != oauth_creds.access_token
            || back.refresh_token != oauth_creds.refresh_token
        {
            return Err("迁移 OAuth 凭证后读回校验失败".to_string());
        }
    }
    if let Some(text) = &legacy.cache_json {
        copy_verified(
            &storage::legacy_cache_file_path(),
            &cache_path(&account.id),
            text,
        )?;
    }
    if let Some(text) = &legacy.history_json {
        copy_verified(
            &history::legacy_history_file_path(),
            &history_path(&account.id),
            text,
        )?;
    }

    // ---- 阶段 2：settings.json 落盘（此后 accounts 键存在，不再重入） ----
    let mut settings = storage::load_settings().unwrap_or_default();
    settings.accounts = vec![account];
    settings.login_method = None;
    storage::save_settings(&settings)?;

    // ---- 阶段 3：删旧（失败只记日志，不丢数据——新位置已校验过） ----
    if legacy.api_key.is_some() {
        if let Err(e) = creds::legacy_delete(creds::SLOT_API_KEY) {
            tracing::warn!("删除旧 api_key 槽位失败: {e}");
        }
    }
    if legacy.web_token.is_some() {
        if let Err(e) = creds::legacy_delete(creds::SLOT_WEB_TOKEN) {
            tracing::warn!("删除旧 web_token 槽位失败: {e}");
        }
    }
    if legacy.web_refresh_token.is_some() {
        if let Err(e) = creds::legacy_delete(creds::SLOT_WEB_REFRESH_TOKEN) {
            tracing::warn!("删除旧 web_refresh_token 槽位失败: {e}");
        }
    }
    if legacy.oauth.is_some() {
        if let Err(e) = oauth::clear_legacy_credentials() {
            tracing::warn!("删除旧 OAuth 凭证文件失败: {e}");
        }
    }
    if legacy.cache_json.is_some() {
        let _ = std::fs::remove_file(storage::legacy_cache_file_path());
    }
    if legacy.history_json.is_some() {
        let _ = std::fs::remove_file(history::legacy_history_file_path());
    }

    tracing::info!("旧单账号数据迁移完成");
    Ok(true)
}

/// 从旧位置收集到的全部单账号数据
#[derive(Default)]
struct LegacyData {
    login_method: Option<String>,
    api_key: Option<String>,
    web_token: Option<String>,
    web_refresh_token: Option<String>,
    oauth: Option<oauth::Credentials>,
    cache_json: Option<String>,
    history_json: Option<String>,
}

impl LegacyData {
    /// 一样都没有 = 全新安装
    fn is_empty(&self) -> bool {
        self.login_method.is_none()
            && self.api_key.is_none()
            && self.web_token.is_none()
            && self.web_refresh_token.is_none()
            && self.oauth.is_none()
            && self.cache_json.is_none()
            && self.history_json.is_none()
    }
}

/// settings.json 是否已含顶层 accounts 键（文件不存在 / 损坏 → false）
fn settings_json_has_accounts_key() -> Result<bool, String> {
    let text = match std::fs::read_to_string(storage::settings_file_path()) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("读取设置失败: {e}")),
    };
    // 损坏文件按"无 accounts 键"处理：load_settings 本就把损坏容忍为默认设置
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(_) => return Ok(false),
    };
    Ok(value.get("accounts").is_some())
}

/// 读出全部旧位置数据；任一读取出错（非"不存在"）即报错中止
fn read_legacy() -> Result<LegacyData, String> {
    let settings = storage::load_settings().unwrap_or_default();
    Ok(LegacyData {
        login_method: settings.login_method,
        api_key: creds::legacy_load(creds::SLOT_API_KEY).map_err(|e| e.to_string())?,
        web_token: creds::legacy_load(creds::SLOT_WEB_TOKEN).map_err(|e| e.to_string())?,
        web_refresh_token: creds::legacy_load(creds::SLOT_WEB_REFRESH_TOKEN)
            .map_err(|e| e.to_string())?,
        oauth: oauth::load_legacy_credentials().map_err(|e| e.to_string())?,
        cache_json: read_text_if_exists(&storage::legacy_cache_file_path())?,
        history_json: read_text_if_exists(&history::legacy_history_file_path())?,
    })
}

/// 读文本文件：不存在 → None
fn read_text_if_exists(path: &std::path::Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("读取 {} 失败: {e}", path.display())),
    }
}

/// 把文本写入新路径并读回校验一致（新位置已存在等价文件则直接通过）
fn copy_verified(_from: &std::path::Path, to: &std::path::Path, text: &str) -> Result<(), String> {
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建配置目录失败: {e}"))?;
    }
    std::fs::write(to, text).map_err(|e| format!("写入 {} 失败: {e}", to.display()))?;
    let back =
        std::fs::read_to_string(to).map_err(|e| format!("读回 {} 失败: {e}", to.display()))?;
    if back != text {
        return Err(format!("迁移 {} 后读回校验失败", to.display()));
    }
    Ok(())
}

fn cache_path(account_id: &str) -> std::path::PathBuf {
    storage::config_dir().join(format!("cache-{account_id}.json"))
}

fn history_path(account_id: &str) -> std::path::PathBuf {
    storage::config_dir().join(format!("history-{account_id}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 环境变量是进程级全局状态，相关测试都须持锁串行（lib.rs::TEST_ENV_LOCK）
    use crate::TEST_ENV_LOCK as ENV_LOCK;

    /// 独立临时配置目录 + 独立 keyring service，绝不碰真实 %APPDATA% 与生产凭据
    fn use_isolated_env() -> (std::path::PathBuf, String) {
        let dir =
            std::env::temp_dir().join(format!("kimicodebar-migrate-test-{}", uuid::Uuid::new_v4()));
        std::env::set_var("KIMICODEBAR_CONFIG_DIR", &dir);
        let service = format!("KimiCodeBar-test-{}", uuid::Uuid::new_v4());
        std::env::set_var("KIMICODEBAR_KEYRING_SERVICE", &service);
        (dir, service)
    }

    fn cleanup(dir: &std::path::Path, service: &str) {
        // 先清 keyring 测试条目（趁 service 环境变量还在），再撤环境变量与临时目录
        for slot in [
            creds::SLOT_API_KEY,
            creds::SLOT_WEB_TOKEN,
            creds::SLOT_WEB_REFRESH_TOKEN,
        ] {
            let _ = keyring::Entry::new(service, slot).map(|e| e.delete_credential());
        }
        let _ = keyring::Entry::new(service, "api_key/acc-dummy").map(|e| e.delete_credential());
        std::env::remove_var("KIMICODEBAR_CONFIG_DIR");
        std::env::remove_var("KIMICODEBAR_KEYRING_SERVICE");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 往旧版裸槽位写一个值（模拟旧版应用留下的凭据）
    fn seed_legacy_slot(service: &str, slot: &str, value: &str) {
        keyring::Entry::new(service, slot)
            .unwrap()
            .set_password(value)
            .unwrap();
    }

    fn legacy_slot(service: &str, slot: &str) -> Option<String> {
        keyring::Entry::new(service, slot)
            .unwrap()
            .get_password()
            .ok()
    }

    #[test]
    fn fresh_install_migrates_nothing() {
        let _guard = ENV_LOCK.lock().unwrap();
        let (dir, service) = use_isolated_env();

        // 全新安装：无任何旧数据 → 不迁移，且不生造 settings.json
        assert!(!migrate_legacy_to_accounts().unwrap());
        assert!(!dir.join("settings.json").exists());

        cleanup(&dir, &service);
    }

    #[test]
    fn migrates_all_legacy_artifacts_and_keeps_data() {
        let _guard = ENV_LOCK.lock().unwrap();
        let (dir, service) = use_isolated_env();
        std::fs::create_dir_all(&dir).unwrap();

        // 旧版现场：settings.json（无 accounts 键）+ 三裸槽位 + credentials.json
        // + cache.json + history.json
        std::fs::write(
            dir.join("settings.json"),
            r#"{"login_method":"api_key","refresh_interval_min":10}"#,
        )
        .unwrap();
        seed_legacy_slot(&service, "api_key", "sk-kimi-legacykey");
        seed_legacy_slot(&service, "web_token", "legacy-kimi-auth");
        seed_legacy_slot(&service, "web_refresh_token", "legacy-refresh");
        std::fs::write(
            dir.join("credentials.json"),
            r#"{"access_token":"legacy-oauth-access","refresh_token":"legacy-oauth-refresh","expires_at":1900000000}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("cache.json"),
            r#"{"quota":{"weekly":{"used":30.0,"limit":100.0,"remaining":70.0,"percent_remaining":70.0}},"fetched_at":1900000000}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("history.json"),
            r#"{"points":[{"t":100,"weekly":5.0}]}"#,
        )
        .unwrap();

        assert!(migrate_legacy_to_accounts().unwrap());

        // settings：accounts=[账号1]，继承旧登录方式，旧全局 login_method 已清
        let settings = storage::load_settings().unwrap();
        assert_eq!(settings.accounts.len(), 1);
        let account = &settings.accounts[0];
        assert_eq!(account.name, "账号 1");
        assert_eq!(account.login_method.as_deref(), Some("api_key"));
        assert!(settings.login_method.is_none());
        // 旧全局字段不丢（refresh_interval_min=10 保留）
        assert_eq!(settings.refresh_interval_min, 10);
        let raw = std::fs::read_to_string(dir.join("settings.json")).unwrap();
        assert!(raw.contains("\"accounts\""));

        // keyring：新槽位值一致，旧裸槽位已删
        assert_eq!(
            creds::load_api_key(&account.id).unwrap().as_deref(),
            Some("sk-kimi-legacykey")
        );
        assert_eq!(
            creds::load_web_token(&account.id).unwrap().as_deref(),
            Some("legacy-kimi-auth")
        );
        assert_eq!(
            creds::load_web_refresh_token(&account.id)
                .unwrap()
                .as_deref(),
            Some("legacy-refresh")
        );
        assert!(legacy_slot(&service, "api_key").is_none());
        assert!(legacy_slot(&service, "web_token").is_none());
        assert!(legacy_slot(&service, "web_refresh_token").is_none());

        // OAuth：新文件能读回同样的 token，旧文件已删
        let creds_back = oauth::load_credentials(&account.id)
            .unwrap()
            .expect("迁移后的 OAuth 凭证应能读回");
        assert_eq!(creds_back.access_token, "legacy-oauth-access");
        assert_eq!(
            creds_back.refresh_token.as_deref(),
            Some("legacy-oauth-refresh")
        );
        assert!(!dir.join("credentials.json").exists());

        // cache / history：内容逐字节一致，旧文件已删
        let cache_text =
            std::fs::read_to_string(dir.join(format!("cache-{}.json", account.id))).unwrap();
        assert!(cache_text.contains("\"fetched_at\":1900000000"));
        assert!(!dir.join("cache.json").exists());
        let history_text =
            std::fs::read_to_string(dir.join(format!("history-{}.json", account.id))).unwrap();
        assert!(history_text.contains("\"weekly\":5.0"));
        assert!(!dir.join("history.json").exists());

        // 幂等：第二次运行为无操作
        assert!(!migrate_legacy_to_accounts().unwrap());

        // 收尾：删掉测试账号的新槽位，再清环境
        creds::clear_api_key(&account.id).unwrap();
        creds::clear_web_token(&account.id).unwrap();
        creds::clear_web_refresh_token(&account.id).unwrap();
        cleanup(&dir, &service);
    }

    #[test]
    fn already_migrated_never_touches_legacy_slots() {
        let _guard = ENV_LOCK.lock().unwrap();
        let (dir, service) = use_isolated_env();
        std::fs::create_dir_all(&dir).unwrap();

        // 已是多账号现场（settings.json 含 accounts 键），但凭据管理器里还躺着旧裸槽位
        // （比如阶段 3 删除失败的残留）：迁移不应重入，更不能动旧槽位
        std::fs::write(dir.join("settings.json"), r#"{"accounts":[]}"#).unwrap();
        seed_legacy_slot(&service, "api_key", "sk-kimi-should-stay");

        assert!(!migrate_legacy_to_accounts().unwrap());
        assert_eq!(
            legacy_slot(&service, "api_key").as_deref(),
            Some("sk-kimi-should-stay")
        );

        cleanup(&dir, &service);
    }
}
