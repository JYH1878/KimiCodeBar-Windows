//! 设置与配额缓存的本地持久化：`settings.json` / `cache.json`。
//!
//! 目录规则与 OAuth 凭证（kimi::oauth）完全一致：
//! `KIMICODEBAR_CONFIG_DIR` 环境变量覆盖（测试/便携模式），否则 `%APPDATA%\KimiCodeBar`。
//! 写入均为原子写（临时文件 + rename）；损坏文件容忍为默认设置 / 无缓存。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::quota::KimiQuota;

/// 默认刷新间隔（分钟）
pub const DEFAULT_REFRESH_INTERVAL_MIN: u32 = 5;
/// 最小刷新间隔（分钟），加载/保存时小于该值会被钳制
pub const MIN_REFRESH_INTERVAL_MIN: u32 = 1;
/// 最大刷新间隔（分钟），加载/保存时大于该值会被钳制
pub const MAX_REFRESH_INTERVAL_MIN: u32 = 60;
/// 默认低额度告警阈值（剩余百分比）
pub const DEFAULT_WARN_THRESHOLD_PCT: f64 = 20.0;
/// DeepSeek 低余额告警阈值默认值（元）
pub const DEFAULT_DEEPSEEK_WARN_THRESHOLD: f64 = 5.0;
/// DeepSeek 低余额告警阈值上限（元，防手改 json 越界）
pub const MAX_DEEPSEEK_WARN_THRESHOLD: f64 = 100_000.0;
/// 账号数量上限（面板一页一个账号 + 末尾「+」）
pub const MAX_ACCOUNTS: usize = 5;

/// 单个账号（settings.json 的 accounts 数组元素，snake_case）。
/// 凭证本体不落盘到这里：API Key / 网页 token 在 Windows 凭据管理器（槽位名带账号 id，
/// 见 creds.rs），OAuth 在每账号一个 DPAPI 文件 credentials-<id>.json（见 kimi::oauth）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Account {
    /// 稳定标识（uuid v4），keyring 槽位 / 文件名 / 内存状态都按它索引
    pub id: String,
    /// 展示名（面板页头、通知文案），默认「账号 N」
    pub name: String,
    /// 登录方式："api_key" / "oauth"；None 表示未显式选择（优先 api_key，其次 oauth）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_method: Option<String>,
    /// 提供商："kimi"（默认）/ "deepseek"；旧版设置文件无此字段，serde 默认 "kimi"
    #[serde(default = "default_provider")]
    pub provider: String,
}

/// Account.provider 缺省值：旧版设置文件无此字段的账号一律按 Kimi 处理
fn default_provider() -> String {
    "kimi".to_string()
}

impl Account {
    /// 是否 DeepSeek 账号（只查余额，无配额/月度/历史）
    pub fn is_deepseek(&self) -> bool {
        self.provider == "deepseek"
    }
}

/// 应用设置（settings.json，snake_case）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    /// 账号列表（面板页顺序 = 列表顺序）；旧版设置文件无此字段，读回空数组，
    /// 由 migrate::migrate_legacy_to_accounts 把旧单账号数据迁移为「账号 1」
    #[serde(default)]
    pub accounts: Vec<Account>,
    /// 【已废弃】全局登录方式已迁移到 Account.login_method；保留字段仅为读取旧版
    /// 设置文件（迁移用），新代码一律不写（None 时不序列化）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_method: Option<String>,
    /// 后台轮询间隔（分钟），默认 5，范围 1–60
    #[serde(default = "default_refresh_interval_min")]
    pub refresh_interval_min: u32,
    /// 自适应刷新：开（默认）时近 10 分钟有 token 消耗按 1 分钟轮询、静默按固定间隔；
    /// 关时恒按 refresh_interval_min 固定间隔
    #[serde(default = "default_adaptive_refresh")]
    pub adaptive_refresh: bool,
    /// 低额度时是否发系统通知，默认开
    #[serde(default = "default_low_warn_enabled")]
    pub low_warn_enabled: bool,
    /// 低额度告警阈值（剩余百分比，严格小于触发），默认 20.0，范围 1–99
    #[serde(default = "default_warn_threshold_pct")]
    pub warn_threshold_pct: f64,
    /// DeepSeek 低余额告警阈值（元，严格小于触发），默认 5.0，范围 0–100000
    #[serde(default = "default_deepseek_warn_threshold")]
    pub deepseek_warn_threshold: f64,
    /// 开机自启动，默认关
    #[serde(default)]
    pub autostart: bool,
    /// 极简模式：开后面板只显示 7 天 / 5 小时额度条（窗口压矮），默认关
    #[serde(default)]
    pub minimal_mode: bool,
    /// 全局热键（如 "Ctrl+Shift+K"），None/空串表示禁用
    #[serde(default)]
    pub hotkey: Option<String>,
    /// 界面语言："system" / "zh" / "en"；None 等同 "system"（跟随系统区域）
    #[serde(default)]
    pub language: Option<String>,
    /// 主题模式："system" / "dark" / "light"；None 等同 "system"（跟随系统明暗）
    #[serde(default)]
    pub theme: Option<String>,
    /// 面板背景图片文件名（如 "background.png"，存于配置目录），None 表示无自定义背景
    #[serde(default)]
    pub background_image: Option<String>,
    /// 预设背景 id（night / aurora / violet / ember），None 表示未选预设。
    /// 生效规则：preset 优先于 image；两者皆 None 为无背景（background.rs 注释有完整互斥说明）
    #[serde(default)]
    pub background_preset: Option<String>,
}

const fn default_refresh_interval_min() -> u32 {
    DEFAULT_REFRESH_INTERVAL_MIN
}

const fn default_adaptive_refresh() -> bool {
    true
}

const fn default_low_warn_enabled() -> bool {
    true
}

const fn default_warn_threshold_pct() -> f64 {
    DEFAULT_WARN_THRESHOLD_PCT
}

const fn default_deepseek_warn_threshold() -> f64 {
    DEFAULT_DEEPSEEK_WARN_THRESHOLD
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            accounts: Vec::new(),
            login_method: None,
            refresh_interval_min: DEFAULT_REFRESH_INTERVAL_MIN,
            adaptive_refresh: true,
            low_warn_enabled: true,
            warn_threshold_pct: DEFAULT_WARN_THRESHOLD_PCT,
            deepseek_warn_threshold: DEFAULT_DEEPSEEK_WARN_THRESHOLD,
            autostart: false,
            minimal_mode: false,
            hotkey: None,
            language: None,
            theme: None,
            background_image: None,
            background_preset: None,
        }
    }
}

impl Settings {
    /// 轮询间隔（秒），供 tokio interval 使用；保证至少 1 分钟
    pub fn refresh_interval_secs(&self) -> u64 {
        u64::from(self.refresh_interval_min.max(MIN_REFRESH_INTERVAL_MIN)) * 60
    }

    /// 按 id 查账号
    pub fn account(&self, account_id: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.id == account_id)
    }

    /// 新增账号：超上限（Kimi + DeepSeek 合计 5 个）报错；名称为空时默认「账号 N」
    /// （N = 当前数量 + 1）。provider 仅识别 "deepseek"，其余一律按 "kimi"（防御性归一）。
    /// 返回新建账号的克隆（id 为 uuid v4）
    pub fn add_account(&mut self, name: Option<&str>, provider: &str) -> Result<Account, String> {
        if self.accounts.len() >= MAX_ACCOUNTS {
            return Err(format!("最多支持 {MAX_ACCOUNTS} 个账号"));
        }
        let name = name
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("账号 {}", self.accounts.len() + 1));
        let account = Account {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            login_method: None,
            provider: if provider == "deepseek" {
                "deepseek".to_string()
            } else {
                default_provider()
            },
        };
        self.accounts.push(account.clone());
        Ok(account)
    }

    /// 改名：名称 trim 后为空报错；账号不存在报错
    pub fn rename_account(&mut self, account_id: &str, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("账号名称不能为空".to_string());
        }
        let Some(account) = self.accounts.iter_mut().find(|a| a.id == account_id) else {
            return Err("账号不存在".to_string());
        };
        account.name = name.to_string();
        Ok(())
    }

    /// 上移/下移一个位次（direction 仅取 -1 / +1 的符号，0 或越界为无操作）。
    /// 返回是否发生了移动
    pub fn move_account(&mut self, account_id: &str, direction: i32) -> bool {
        let Some(index) = self.accounts.iter().position(|a| a.id == account_id) else {
            return false;
        };
        let target = index as i32 + direction.signum();
        if direction == 0 || target < 0 || target >= self.accounts.len() as i32 {
            return false;
        }
        self.accounts.swap(index, target as usize);
        true
    }

    /// 删除账号（仅移除设置项；凭证/缓存等残留清理由调用方负责），返回被删账号
    pub fn remove_account(&mut self, account_id: &str) -> Option<Account> {
        let index = self.accounts.iter().position(|a| a.id == account_id)?;
        Some(self.accounts.remove(index))
    }
}

/// 最近一次成功拉取的配额缓存（cache.json）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedQuota {
    pub quota: KimiQuota,
    /// 拉取成功时间（epoch 秒）
    pub fetched_at: i64,
    /// 月度总量缓存（网页 token 数据）；旧版缓存文件无此字段，向后兼容
    #[serde(default)]
    pub monthly: Option<crate::kimi::web::MonthlyInfo>,
    /// DeepSeek 余额缓存（仅 provider=deepseek 的账号有值）；旧版缓存文件无此字段，向后兼容
    #[serde(default)]
    pub deepseek_balance: Option<crate::deepseek::models::DeepSeekBalance>,
}

/// 读取设置：文件不存在 → 默认；损坏 → 默认；其他 IO 错误 → Err。
/// 加载后刷新间隔钳制到 1–60 分钟、告警阈值钳制到 1–99、DeepSeek 阈值钳制到
/// 0–100000 元（防手改 json 越界）。
pub fn load_settings() -> Result<Settings, String> {
    let path = settings_file_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Settings::default()),
        Err(e) => return Err(format!("读取设置失败: {e}")),
    };
    // 损坏文件容忍为默认（与 oauth::load_credentials 的语义一致）
    let mut settings: Settings = serde_json::from_str(&text).unwrap_or_default();
    settings.refresh_interval_min = settings
        .refresh_interval_min
        .clamp(MIN_REFRESH_INTERVAL_MIN, MAX_REFRESH_INTERVAL_MIN);
    settings.warn_threshold_pct = settings.warn_threshold_pct.clamp(1.0, 99.0);
    settings.deepseek_warn_threshold = settings
        .deepseek_warn_threshold
        .clamp(0.0, MAX_DEEPSEEK_WARN_THRESHOLD);
    Ok(settings)
}

/// 原子写入 settings.json（临时文件 + rename）
pub fn save_settings(settings: &Settings) -> Result<(), String> {
    save_json(&settings_file_path(), "settings.json.tmp", settings)
}

/// 读取配额缓存（每账号一个文件 cache-<id>.json）：文件不存在或损坏均为 None；
/// 其他 IO 错误同样按 None 容忍（缓存不是关键数据，丢了重新拉即可）
pub fn load_cache(account_id: &str) -> Option<CachedQuota> {
    let text = std::fs::read_to_string(cache_file_path(account_id)).ok()?;
    serde_json::from_str(&text).ok()
}

/// 原子写入该账号的 cache-<id>.json（临时文件 + rename）
pub fn save_cache(account_id: &str, cache: &CachedQuota) -> Result<(), String> {
    save_json(
        &cache_file_path(account_id),
        &format!("cache-{account_id}.json.tmp"),
        cache,
    )
}

// ---------------------------------------------------------------------------
// 以下为内部实现
// ---------------------------------------------------------------------------

/// 序列化为 pretty JSON 后原子写入（先删目标再 rename，Windows rename 不允许覆盖）
fn save_json<T: Serialize>(path: &PathBuf, tmp_name: &str, value: &T) -> Result<(), String> {
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| format!("创建配置目录失败: {e}"))?;

    let json = serde_json::to_string_pretty(value).map_err(|e| format!("序列化失败: {e}"))?;
    let tmp_path = dir.join(tmp_name);
    std::fs::write(&tmp_path, json).map_err(|e| format!("写入临时文件失败: {e}"))?;
    if path.exists() {
        std::fs::remove_file(path).map_err(|e| format!("删除旧文件失败: {e}"))?;
    }
    std::fs::rename(&tmp_path, path).map_err(|e| format!("重命名临时文件失败: {e}"))
}

/// 配置目录：KIMICODEBAR_CONFIG_DIR 覆盖，否则 %APPDATA%\KimiCodeBar
/// （与 kimi::oauth::config_dir 保持一致，两份实现需同步修改）
pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("KIMICODEBAR_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return PathBuf::from(appdata).join("KimiCodeBar");
    }
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        return PathBuf::from(home)
            .join("AppData")
            .join("Roaming")
            .join("KimiCodeBar");
    }
    std::env::temp_dir().join("KimiCodeBar")
}

/// settings.json 路径（pub(crate)：迁移模块要读原文判断有无 accounts 键）
pub(crate) fn settings_file_path() -> PathBuf {
    config_dir().join("settings.json")
}

/// 该账号的配额缓存路径：{config_dir}/cache-<id>.json
/// （pub(crate)：删账号清理残留用，见 accounts::purge_account_data）
pub(crate) fn cache_file_path(account_id: &str) -> PathBuf {
    config_dir().join(format!("cache-{account_id}.json"))
}

/// 旧单账号时代的配额缓存路径（仅迁移用）：{config_dir}/cache.json
pub(crate) fn legacy_cache_file_path() -> PathBuf {
    config_dir().join("cache.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    // 环境变量是进程级全局状态，凡改动 KIMICODEBAR_CONFIG_DIR 的测试都须持锁串行；
    // 锁为全库共享（lib.rs::TEST_ENV_LOCK），与 kimi::oauth 等模块的同类测试互斥
    use crate::TEST_ENV_LOCK as ENV_LOCK;

    /// 指向独立临时目录，避免碰真实 %APPDATA%
    fn use_temp_config_dir() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("kimicodebar-storage-test-{}", uuid::Uuid::new_v4()));
        std::env::set_var("KIMICODEBAR_CONFIG_DIR", &dir);
        dir
    }

    fn cleanup(dir: &PathBuf) {
        std::env::remove_var("KIMICODEBAR_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn settings_missing_file_returns_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_config_dir();

        assert_eq!(load_settings().unwrap(), Settings::default());

        cleanup(&dir);
    }

    #[test]
    fn settings_save_load_roundtrip() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_config_dir();

        let settings = Settings {
            accounts: vec![Account {
                id: "acc-1".to_string(),
                name: "账号 1".to_string(),
                login_method: Some("oauth".to_string()),
                provider: "deepseek".to_string(),
            }],
            login_method: Some("oauth".to_string()),
            refresh_interval_min: 15,
            adaptive_refresh: false,
            low_warn_enabled: false,
            warn_threshold_pct: 33.5,
            deepseek_warn_threshold: 12.5,
            autostart: true,
            // 顺手覆盖极简模式的落盘/读回（true 值往返一致）
            minimal_mode: true,
            hotkey: Some("Ctrl+Shift+K".to_string()),
            language: Some("zh".to_string()),
            theme: Some("light".to_string()),
            background_image: Some("background.png".to_string()),
            background_preset: None,
        };
        save_settings(&settings).unwrap();
        assert!(dir.join("settings.json").exists());
        // 临时文件不应残留
        assert!(!dir.join("settings.json.tmp").exists());

        assert_eq!(load_settings().unwrap(), settings);

        // 覆盖写入（rename 目标已存在的路径）
        let updated = Settings {
            login_method: Some("api_key".to_string()),
            ..settings.clone()
        };
        save_settings(&updated).unwrap();
        assert_eq!(
            load_settings().unwrap().login_method.as_deref(),
            Some("api_key")
        );

        // 磁盘格式为 snake_case JSON
        let raw = std::fs::read_to_string(dir.join("settings.json")).unwrap();
        assert!(raw.contains("\"accounts\""));
        assert!(raw.contains("\"login_method\""));
        assert!(raw.contains("\"refresh_interval_min\""));
        assert!(raw.contains("\"low_warn_enabled\""));
        assert!(raw.contains("\"warn_threshold_pct\""));
        // provider 随账号落盘（该用例账号为 deepseek，顺带覆盖序列化）
        assert!(raw.contains("\"provider\""));
        assert!(raw.contains("\"deepseek\""));
        assert!(raw.contains("\"deepseek_warn_threshold\""));

        cleanup(&dir);
    }

    #[test]
    fn settings_corrupt_file_returns_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_config_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("settings.json"), "not json").unwrap();

        assert_eq!(load_settings().unwrap(), Settings::default());

        cleanup(&dir);
    }

    #[test]
    fn settings_minimal_mode_defaults_to_false() {
        // 新字段默认值：缺省构造为关（普通模式）
        assert!(!Settings::default().minimal_mode);
    }

    #[test]
    fn settings_legacy_json_without_minimal_mode_loads() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_config_dir();
        std::fs::create_dir_all(&dir).unwrap();
        // 旧版设置文件无 minimal_mode 字段：#[serde(default)] 读回 false，其余字段不受影响
        std::fs::write(dir.join("settings.json"), r#"{"refresh_interval_min":10}"#).unwrap();

        let settings = load_settings().unwrap();
        assert_eq!(settings.refresh_interval_min, 10);
        assert!(!settings.minimal_mode);

        cleanup(&dir);
    }

    #[test]
    fn settings_partial_json_fills_defaults() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_config_dir();
        std::fs::create_dir_all(&dir).unwrap();
        // 只有部分字段：其余字段按默认填充
        std::fs::write(dir.join("settings.json"), r#"{"login_method":"oauth"}"#).unwrap();

        let settings = load_settings().unwrap();
        assert_eq!(settings.login_method.as_deref(), Some("oauth"));
        // 旧版设置文件无 accounts 字段：#[serde(default)] 读回空数组
        assert!(settings.accounts.is_empty());
        assert_eq!(settings.refresh_interval_min, DEFAULT_REFRESH_INTERVAL_MIN);
        // 旧版设置文件无 adaptive_refresh 字段：读回默认 true（自适应）
        assert!(settings.adaptive_refresh);
        assert!(settings.low_warn_enabled);
        assert_eq!(settings.warn_threshold_pct, DEFAULT_WARN_THRESHOLD_PCT);
        assert!(!settings.autostart);
        // 旧版设置文件无 hotkey 字段：#[serde(default)] 读回 None
        assert!(settings.hotkey.is_none());
        // 旧版设置文件无 language 字段：读回 None（等同 "system" 跟随系统）
        assert!(settings.language.is_none());
        // 旧版设置文件无 theme 字段：读回 None（等同 "system" 跟随系统明暗）
        assert!(settings.theme.is_none());
        // 旧版设置文件无 background_image 字段：读回 None（无自定义背景）
        assert!(settings.background_image.is_none());
        // 旧版设置文件无 background_preset 字段：读回 None（未选预设背景）
        assert!(settings.background_preset.is_none());

        cleanup(&dir);
    }

    #[test]
    fn settings_refresh_interval_clamped_to_min_1() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_config_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("settings.json"), r#"{"refresh_interval_min":0}"#).unwrap();

        let settings = load_settings().unwrap();
        assert_eq!(settings.refresh_interval_min, 1);
        assert_eq!(settings.refresh_interval_secs(), 60);

        cleanup(&dir);
    }

    #[test]
    fn settings_out_of_range_values_clamped_on_load() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_config_dir();
        std::fs::create_dir_all(&dir).unwrap();
        // 手改 json 越界：刷新间隔 >60、阈值 <1 / >99 都在加载时钳回范围内
        std::fs::write(
            dir.join("settings.json"),
            r#"{"refresh_interval_min":3600,"warn_threshold_pct":0}"#,
        )
        .unwrap();

        let settings = load_settings().unwrap();
        assert_eq!(settings.refresh_interval_min, MAX_REFRESH_INTERVAL_MIN);
        assert_eq!(settings.warn_threshold_pct, 1.0);

        std::fs::write(dir.join("settings.json"), r#"{"warn_threshold_pct":150}"#).unwrap();
        assert_eq!(load_settings().unwrap().warn_threshold_pct, 99.0);

        cleanup(&dir);
    }

    #[test]
    fn cache_save_load_roundtrip() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_config_dir();
        let acc = "acc-cache";

        // 未保存时读取为 None
        assert!(load_cache(acc).is_none());

        let quota = crate::quota::parse_usage(r#"{"usage":{"limit":"100","used":"30"}}"#).unwrap();
        let cache = CachedQuota {
            quota,
            fetched_at: 1_900_000_000,
            monthly: None,
            deepseek_balance: None,
        };
        save_cache(acc, &cache).unwrap();
        assert!(dir.join("cache-acc-cache.json").exists());
        assert!(!dir.join("cache-acc-cache.json.tmp").exists());

        let loaded = load_cache(acc).expect("应能读回缓存");
        assert_eq!(loaded.fetched_at, 1_900_000_000);
        assert!(loaded.monthly.is_none());
        let weekly = loaded.quota.weekly.as_ref().expect("weekly 应存在");
        assert_eq!(weekly.limit, 100.0);
        assert!((weekly.percent_remaining - 70.0).abs() < 1e-9);

        // 覆盖写入
        save_cache(
            acc,
            &CachedQuota {
                quota: loaded.quota.clone(),
                fetched_at: 1_900_000_100,
                monthly: None,
                deepseek_balance: None,
            },
        )
        .unwrap();
        assert_eq!(load_cache(acc).unwrap().fetched_at, 1_900_000_100);

        // 磁盘格式为 snake_case JSON（与 types.ts 契约一致）
        let raw = std::fs::read_to_string(dir.join("cache-acc-cache.json")).unwrap();
        assert!(raw.contains("\"fetched_at\""));
        assert!(raw.contains("\"percent_remaining\""));

        cleanup(&dir);
    }

    #[test]
    fn cache_corrupt_file_returns_none() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_config_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cache-acc-x.json"), "not json").unwrap();

        assert!(load_cache("acc-x").is_none());

        cleanup(&dir);
    }

    #[test]
    fn cache_monthly_roundtrip_and_backward_compat() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_config_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let acc = "acc-m";

        // 旧版缓存文件没有 monthly 字段：#[serde(default)] 保证读回 None
        std::fs::write(
            dir.join("cache-acc-m.json"),
            r#"{"quota":{},"fetched_at":1900000000}"#,
        )
        .unwrap();
        let loaded = load_cache(acc).expect("旧格式缓存应能读回");
        assert!(loaded.monthly.is_none());

        // 带月度数据写入 → 读回一致
        let monthly = crate::kimi::web::MonthlyInfo {
            total_pct: 16.12,
            kimi_pct: 11.12,
            code_pct: 5.0,
            reset_time: Some("2026-08-01T00:00:00Z".to_string()),
        };
        save_cache(
            acc,
            &CachedQuota {
                quota: crate::quota::KimiQuota::default(),
                fetched_at: 1_900_000_000,
                monthly: Some(monthly),
                deepseek_balance: None,
            },
        )
        .unwrap();
        let loaded = load_cache(acc).expect("应能读回缓存");
        let m = loaded.monthly.expect("monthly 应存在");
        assert!((m.total_pct - 16.12).abs() < 1e-9);
        assert!((m.kimi_pct - 11.12).abs() < 1e-9);
        assert!((m.code_pct - 5.0).abs() < 1e-9);
        assert_eq!(m.reset_time.as_deref(), Some("2026-08-01T00:00:00Z"));

        cleanup(&dir);
    }

    // ---- 账号列表操作 ----

    #[test]
    fn add_account_default_name_and_cap() {
        let mut settings = Settings::default();
        let a1 = settings.add_account(None, "kimi").unwrap();
        assert_eq!(a1.name, "账号 1");
        assert!(!a1.id.is_empty());
        assert_eq!(a1.provider, "kimi");
        let a2 = settings.add_account(Some("  工作号  "), "kimi").unwrap();
        assert_eq!(a2.name, "工作号");
        assert_ne!(a1.id, a2.id);
        // 默认名 N = 当前数量 + 1
        let a3 = settings.add_account(Some(""), "kimi").unwrap();
        assert_eq!(a3.name, "账号 3");
        settings.add_account(None, "kimi").unwrap();
        settings.add_account(None, "kimi").unwrap();
        // 第 6 个超上限
        assert!(settings.add_account(None, "kimi").is_err());
        assert_eq!(settings.accounts.len(), MAX_ACCOUNTS);
    }

    #[test]
    fn add_account_provider_normalized_and_cap_counts_deepseek() {
        let mut settings = Settings::default();
        // provider 归一：仅 "deepseek" 识别为 DeepSeek，其余（含未知值）按 kimi
        let ds = settings.add_account(Some("DS"), "deepseek").unwrap();
        assert_eq!(ds.provider, "deepseek");
        assert!(ds.is_deepseek());
        let unknown = settings.add_account(Some("X"), "something-else").unwrap();
        assert_eq!(unknown.provider, "kimi");
        assert!(!unknown.is_deepseek());

        // 上限按 Kimi + DeepSeek 合计校验：已有 2 个，再补 3 个到 5，第 6 个（deepseek）报错
        settings.add_account(None, "kimi").unwrap();
        settings.add_account(None, "deepseek").unwrap();
        settings.add_account(None, "kimi").unwrap();
        assert_eq!(settings.accounts.len(), MAX_ACCOUNTS);
        assert!(settings.add_account(None, "deepseek").is_err());
    }

    #[test]
    fn rename_account_trims_and_validates() {
        let mut settings = Settings::default();
        let a = settings.add_account(None, "kimi").unwrap();
        settings.rename_account(&a.id, "  主号 ").unwrap();
        assert_eq!(settings.account(&a.id).unwrap().name, "主号");
        assert!(settings.rename_account(&a.id, "   ").is_err());
        assert!(settings.rename_account("no-such-id", "x").is_err());
        // 失败不改名
        assert_eq!(settings.account(&a.id).unwrap().name, "主号");
    }

    #[test]
    fn move_account_swaps_with_neighbor() {
        let mut settings = Settings::default();
        let a = settings.add_account(Some("a"), "kimi").unwrap();
        let b = settings.add_account(Some("b"), "kimi").unwrap();
        let c = settings.add_account(Some("c"), "kimi").unwrap();

        // 上移第二位 → 与第一位交换
        assert!(settings.move_account(&b.id, -1));
        let names: Vec<&str> = settings.accounts.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, ["b", "a", "c"]);

        // 顶部再上移 / 底部再下移：无操作
        assert!(!settings.move_account(&b.id, -1));
        assert!(!settings.move_account(&c.id, 1));
        // direction 0 / 未知 id：无操作
        assert!(!settings.move_account(&a.id, 0));
        assert!(!settings.move_account("no-such-id", 1));

        // 下移
        assert!(settings.move_account(&b.id, 1));
        let names: Vec<&str> = settings.accounts.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);
    }

    #[test]
    fn remove_account_returns_removed() {
        let mut settings = Settings::default();
        let a = settings.add_account(Some("a"), "kimi").unwrap();
        let b = settings.add_account(Some("b"), "kimi").unwrap();
        let removed = settings.remove_account(&a.id).unwrap();
        assert_eq!(removed.name, "a");
        assert!(settings.account(&a.id).is_none());
        assert_eq!(settings.accounts.len(), 1);
        assert_eq!(settings.accounts[0].id, b.id);
        assert!(settings.remove_account("no-such-id").is_none());
    }

    // ---- provider 字段（DeepSeek 账号）----

    #[test]
    fn settings_legacy_json_without_provider_loads_as_kimi() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_config_dir();
        std::fs::create_dir_all(&dir).unwrap();
        // 旧版设置文件的账号无 provider 字段：serde 默认读回 "kimi"，其余字段不受影响
        std::fs::write(
            dir.join("settings.json"),
            r#"{"accounts":[{"id":"acc-1","name":"账号 1","login_method":"api_key"}],"refresh_interval_min":10}"#,
        )
        .unwrap();

        let settings = load_settings().unwrap();
        assert_eq!(settings.accounts.len(), 1);
        assert_eq!(settings.accounts[0].provider, "kimi");
        assert!(!settings.accounts[0].is_deepseek());
        assert_eq!(
            settings.accounts[0].login_method.as_deref(),
            Some("api_key")
        );
        assert_eq!(settings.refresh_interval_min, 10);

        cleanup(&dir);
    }

    #[test]
    fn deepseek_warn_threshold_default_and_clamped_on_load() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_config_dir();
        std::fs::create_dir_all(&dir).unwrap();

        // 旧版设置文件无该字段：读回默认 5 元
        std::fs::write(dir.join("settings.json"), r#"{"refresh_interval_min":10}"#).unwrap();
        assert_eq!(
            load_settings().unwrap().deepseek_warn_threshold,
            DEFAULT_DEEPSEEK_WARN_THRESHOLD
        );

        // 手改 json 越界：负数钳回 0，超大钳回上限
        std::fs::write(
            dir.join("settings.json"),
            r#"{"deepseek_warn_threshold":-3}"#,
        )
        .unwrap();
        assert_eq!(load_settings().unwrap().deepseek_warn_threshold, 0.0);
        std::fs::write(
            dir.join("settings.json"),
            r#"{"deepseek_warn_threshold":99999999}"#,
        )
        .unwrap();
        assert_eq!(
            load_settings().unwrap().deepseek_warn_threshold,
            MAX_DEEPSEEK_WARN_THRESHOLD
        );

        cleanup(&dir);
    }

    #[test]
    fn cache_deepseek_balance_roundtrip_and_backward_compat() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_config_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let acc = "acc-ds";

        // 旧版缓存文件没有 deepseek_balance 字段：#[serde(default)] 读回 None
        std::fs::write(
            dir.join("cache-acc-ds.json"),
            r#"{"quota":{},"fetched_at":1900000000}"#,
        )
        .unwrap();
        let loaded = load_cache(acc).expect("旧格式缓存应能读回");
        assert!(loaded.deepseek_balance.is_none());

        // 带余额写入 → 读回一致（金额 f64，snake_case 落盘）
        let balance = crate::deepseek::models::DeepSeekBalance {
            is_available: true,
            currency: "CNY".to_string(),
            total_balance: 12.34,
            granted_balance: 2.0,
            topped_up_balance: 10.34,
        };
        save_cache(
            acc,
            &CachedQuota {
                quota: crate::quota::KimiQuota::default(),
                fetched_at: 1_900_000_000,
                monthly: None,
                deepseek_balance: Some(balance),
            },
        )
        .unwrap();
        let loaded = load_cache(acc).expect("应能读回缓存");
        let b = loaded.deepseek_balance.expect("deepseek_balance 应存在");
        assert!(b.is_available);
        assert_eq!(b.currency, "CNY");
        assert!((b.total_balance - 12.34).abs() < 1e-9);
        assert!((b.granted_balance - 2.0).abs() < 1e-9);
        assert!((b.topped_up_balance - 10.34).abs() < 1e-9);
        let raw = std::fs::read_to_string(dir.join("cache-acc-ds.json")).unwrap();
        assert!(raw.contains("\"deepseek_balance\""));
        assert!(raw.contains("\"total_balance\""));

        cleanup(&dir);
    }
}
