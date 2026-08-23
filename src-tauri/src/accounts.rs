//! 账号级数据清理：删除账号时抹掉它在本地的一切残留。
//!
//! 清理范围：keyring 四槽位（api_key / web_token / web_refresh_token / api_key_extra）、
//! OAuth DPAPI 凭证文件、配额缓存、用量历史。
//! 全部尽力而为：单项失败只记日志，继续清其余项——删账号不应被单个坏文件卡住。

use crate::creds;
use crate::kimi::oauth;

/// 抹掉该账号的全部本地数据（设置里的 accounts 条目由调用方负责移除）。
/// 单项清理失败只记 warn 不中断；本函数本身不返回错误。
pub fn purge_account_data(account_id: &str) {
    for (label, result) in [
        ("api_key", creds::clear_api_key(account_id)),
        ("api_key_extra", creds::clear_api_key_extra(account_id)),
        ("web_token", creds::clear_web_token(account_id)),
        (
            "web_refresh_token",
            creds::clear_web_refresh_token(account_id),
        ),
    ] {
        if let Err(e) = result {
            tracing::warn!("删除账号 {account_id} 的 {label} 槽位失败: {e}");
        }
    }
    if let Err(e) = oauth::clear_credentials(account_id) {
        tracing::warn!("删除账号 {account_id} 的 OAuth 凭证失败: {e}");
    }
    for path in [
        crate::storage::cache_file_path(account_id),
        crate::history::history_file_path(account_id),
    ] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            // 本来就不存在也算干净
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("删除账号文件 {} 失败: {e}", path.display()),
        }
    }
    tracing::info!("账号 {account_id} 的本地数据已清理");
}

#[cfg(test)]
mod tests {
    // 环境变量是进程级全局状态，相关测试都须持锁串行（lib.rs::TEST_ENV_LOCK）
    use crate::TEST_ENV_LOCK as ENV_LOCK;

    /// 独立临时配置目录 + 独立 keyring service，绝不碰真实 %APPDATA% 与生产凭据
    fn use_isolated_env() -> (std::path::PathBuf, String) {
        let dir = std::env::temp_dir().join(format!(
            "kimicodebar-accounts-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::env::set_var("KIMICODEBAR_CONFIG_DIR", &dir);
        let service = format!("KimiCodeBar-test-{}", uuid::Uuid::new_v4());
        std::env::set_var("KIMICODEBAR_KEYRING_SERVICE", &service);
        (dir, service)
    }

    fn cleanup(dir: &std::path::Path, service: &str) {
        // 先清 keyring 测试条目（趁 service 环境变量还在），再撤环境变量与临时目录
        for slot in [
            "api_key/acc-a",
            "api_key_extra/acc-a",
            "api_key_extra/acc-b",
            "web_token/acc-a",
            "web_refresh_token/acc-a",
            "api_key/acc-b",
        ] {
            let _ = keyring::Entry::new(service, slot).map(|e| e.delete_credential());
        }
        std::env::remove_var("KIMICODEBAR_CONFIG_DIR");
        std::env::remove_var("KIMICODEBAR_KEYRING_SERVICE");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 反向验证场景之一：删账号后 keyring 槽位与 history 等文件无残留，且不伤其他账号
    #[test]
    fn purge_removes_all_residue_and_spares_other_accounts() {
        let _guard = ENV_LOCK.lock().unwrap();
        let (dir, service) = use_isolated_env();
        std::fs::create_dir_all(&dir).unwrap();

        // 布置 acc-a 的全套数据 + acc-b 的 api_key / api_key_extra（用于验证不误伤）
        crate::creds::save_api_key("acc-a", "key-a").unwrap();
        crate::creds::save_api_key_extra("acc-a", &["extra-a1".to_string()]).unwrap();
        crate::creds::save_web_token("acc-a", "web-a").unwrap();
        crate::creds::save_web_refresh_token("acc-a", "refresh-a").unwrap();
        crate::creds::save_api_key("acc-b", "key-b").unwrap();
        crate::creds::save_api_key_extra("acc-b", &["extra-b1".to_string()]).unwrap();
        crate::kimi::oauth::save_credentials(
            "acc-a",
            &crate::kimi::oauth::Credentials {
                access_token: "oauth-access".to_string(),
                refresh_token: Some("oauth-refresh".to_string()),
                expires_at: Some(1_900_000_000),
                scope: None,
                token_type: None,
            },
        )
        .unwrap();
        std::fs::write(dir.join("cache-acc-a.json"), "{}").unwrap();
        std::fs::write(dir.join("history-acc-a.json"), "{}").unwrap();

        super::purge_account_data("acc-a");

        // acc-a 荡然无存
        assert!(crate::creds::load_api_key("acc-a").unwrap().is_none());
        assert!(crate::creds::load_api_key_extra("acc-a")
            .unwrap()
            .is_empty());
        assert!(crate::creds::load_web_token("acc-a").unwrap().is_none());
        assert!(crate::creds::load_web_refresh_token("acc-a")
            .unwrap()
            .is_none());
        assert!(crate::kimi::oauth::load_credentials("acc-a")
            .unwrap()
            .is_none());
        assert!(!dir.join("cache-acc-a.json").exists());
        assert!(!dir.join("history-acc-a.json").exists());
        // acc-b 不受影响（含额外 key 槽位）
        assert_eq!(
            crate::creds::load_api_key("acc-b").unwrap().as_deref(),
            Some("key-b")
        );
        assert_eq!(
            crate::creds::load_api_key_extra("acc-b").unwrap(),
            vec!["extra-b1".to_string()]
        );

        // 幂等：对已经没有数据的账号再清一次不 panic、不报错
        super::purge_account_data("acc-a");
        super::purge_account_data("acc-never-existed");

        cleanup(&dir, &service);
    }
}
