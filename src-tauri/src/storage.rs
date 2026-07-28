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

/// 应用设置（settings.json，snake_case）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    /// 登录方式："api_key" / "oauth"；None 表示未显式选择（优先 api_key，其次 oauth）
    #[serde(default)]
    pub login_method: Option<String>,
    /// 后台轮询间隔（分钟），默认 5，范围 1–60
    #[serde(default = "default_refresh_interval_min")]
    pub refresh_interval_min: u32,
    /// 低额度时是否发系统通知，默认开
    #[serde(default = "default_low_warn_enabled")]
    pub low_warn_enabled: bool,
    /// 低额度告警阈值（剩余百分比，严格小于触发），默认 20.0，范围 1–99
    #[serde(default = "default_warn_threshold_pct")]
    pub warn_threshold_pct: f64,
    /// 开机自启动，默认关
    #[serde(default)]
    pub autostart: bool,
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

const fn default_low_warn_enabled() -> bool {
    true
}

const fn default_warn_threshold_pct() -> f64 {
    DEFAULT_WARN_THRESHOLD_PCT
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            login_method: None,
            refresh_interval_min: DEFAULT_REFRESH_INTERVAL_MIN,
            low_warn_enabled: true,
            warn_threshold_pct: DEFAULT_WARN_THRESHOLD_PCT,
            autostart: false,
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
}

/// 读取设置：文件不存在 → 默认；损坏 → 默认；其他 IO 错误 → Err。
/// 加载后刷新间隔钳制到 1–60 分钟、告警阈值钳制到 1–99（防手改 json 越界）。
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
    Ok(settings)
}

/// 原子写入 settings.json（临时文件 + rename）
pub fn save_settings(settings: &Settings) -> Result<(), String> {
    save_json(&settings_file_path(), "settings.json.tmp", settings)
}

/// 读取配额缓存：文件不存在或损坏均为 None；其他 IO 错误同样按 None 容忍
/// （缓存不是关键数据，丢了重新拉即可）
pub fn load_cache() -> Option<CachedQuota> {
    let text = std::fs::read_to_string(cache_file_path()).ok()?;
    serde_json::from_str(&text).ok()
}

/// 原子写入 cache.json（临时文件 + rename）
pub fn save_cache(cache: &CachedQuota) -> Result<(), String> {
    save_json(&cache_file_path(), "cache.json.tmp", cache)
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

fn settings_file_path() -> PathBuf {
    config_dir().join("settings.json")
}

fn cache_file_path() -> PathBuf {
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
            login_method: Some("oauth".to_string()),
            refresh_interval_min: 15,
            low_warn_enabled: false,
            warn_threshold_pct: 33.5,
            autostart: true,
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
        assert!(raw.contains("\"login_method\""));
        assert!(raw.contains("\"refresh_interval_min\""));
        assert!(raw.contains("\"low_warn_enabled\""));
        assert!(raw.contains("\"warn_threshold_pct\""));

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
    fn settings_partial_json_fills_defaults() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_config_dir();
        std::fs::create_dir_all(&dir).unwrap();
        // 只有部分字段：其余字段按默认填充
        std::fs::write(dir.join("settings.json"), r#"{"login_method":"oauth"}"#).unwrap();

        let settings = load_settings().unwrap();
        assert_eq!(settings.login_method.as_deref(), Some("oauth"));
        assert_eq!(settings.refresh_interval_min, DEFAULT_REFRESH_INTERVAL_MIN);
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

        // 未保存时读取为 None
        assert!(load_cache().is_none());

        let quota = crate::quota::parse_usage(r#"{"usage":{"limit":"100","used":"30"}}"#).unwrap();
        let cache = CachedQuota {
            quota,
            fetched_at: 1_900_000_000,
            monthly: None,
        };
        save_cache(&cache).unwrap();
        assert!(dir.join("cache.json").exists());
        assert!(!dir.join("cache.json.tmp").exists());

        let loaded = load_cache().expect("应能读回缓存");
        assert_eq!(loaded.fetched_at, 1_900_000_000);
        assert!(loaded.monthly.is_none());
        let weekly = loaded.quota.weekly.as_ref().expect("weekly 应存在");
        assert_eq!(weekly.limit, 100.0);
        assert!((weekly.percent_remaining - 70.0).abs() < 1e-9);

        // 覆盖写入
        save_cache(&CachedQuota {
            quota: loaded.quota.clone(),
            fetched_at: 1_900_000_100,
            monthly: None,
        })
        .unwrap();
        assert_eq!(load_cache().unwrap().fetched_at, 1_900_000_100);

        // 磁盘格式为 snake_case JSON（与 types.ts 契约一致）
        let raw = std::fs::read_to_string(dir.join("cache.json")).unwrap();
        assert!(raw.contains("\"fetched_at\""));
        assert!(raw.contains("\"percent_remaining\""));

        cleanup(&dir);
    }

    #[test]
    fn cache_corrupt_file_returns_none() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_config_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cache.json"), "not json").unwrap();

        assert!(load_cache().is_none());

        cleanup(&dir);
    }

    #[test]
    fn cache_monthly_roundtrip_and_backward_compat() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_config_dir();
        std::fs::create_dir_all(&dir).unwrap();

        // 旧版缓存文件没有 monthly 字段：#[serde(default)] 保证读回 None
        std::fs::write(
            dir.join("cache.json"),
            r#"{"quota":{},"fetched_at":1900000000}"#,
        )
        .unwrap();
        let loaded = load_cache().expect("旧格式缓存应能读回");
        assert!(loaded.monthly.is_none());

        // 带月度数据写入 → 读回一致
        let monthly = crate::kimi::web::MonthlyInfo {
            total_pct: 16.12,
            kimi_pct: 11.12,
            code_pct: 5.0,
            reset_time: Some("2026-08-01T00:00:00Z".to_string()),
        };
        save_cache(&CachedQuota {
            quota: crate::quota::KimiQuota::default(),
            fetched_at: 1_900_000_000,
            monthly: Some(monthly),
        })
        .unwrap();
        let loaded = load_cache().expect("应能读回缓存");
        let m = loaded.monthly.expect("monthly 应存在");
        assert!((m.total_pct - 16.12).abs() < 1e-9);
        assert!((m.kimi_pct - 11.12).abs() < 1e-9);
        assert!((m.code_pct - 5.0).abs() < 1e-9);
        assert_eq!(m.reset_time.as_deref(), Some("2026-08-01T00:00:00Z"));

        cleanup(&dir);
    }
}
