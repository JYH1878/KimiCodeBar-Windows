//! Tauri 命令层与刷新编排（bin 侧）：面板状态组装、防重入刷新、托盘/事件联动。
//!
//! 前端契约（`src/types.ts`）：`get_panel_state()` / `refresh_now()` → PanelState，
//! 后端状态变化时 emit `quota-updated`（payload 为 PanelState JSON）。

use std::sync::Mutex;

use kimicodebar::creds;
use kimicodebar::history;
use kimicodebar::kimi::client::KimiClient;
use kimicodebar::kimi::oauth;
use kimicodebar::kimi::web::{self, MonthlyInfo, WebError};
use kimicodebar::quota::{needs_low_warning, KimiQuota, QuotaError};
use kimicodebar::storage::{self, CachedQuota};
use kimicodebar::update;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_autostart::ManagerExt;
use tokio::sync::watch;

use crate::i18n;
use crate::tray;

/// 面板距上次成功刷新超过该秒数，再次显示时触发后台刷新
const STALE_SECS: i64 = 60;

/// 应用设置（与 src/types.ts 的 AppSettings 一一对应，snake_case；Deserialize 用于收参）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    /// 登录方式："api_key" / "oauth"；None 表示未显式选择（优先 api_key，其次 oauth）
    pub login_method: Option<String>,
    /// 自动刷新间隔（分钟，最小 1，默认 5）
    pub refresh_interval_min: u32,
    /// 低额度告警开关
    pub low_warn_enabled: bool,
    /// 告警阈值（剩余百分比）
    pub warn_threshold_pct: f64,
    /// 开机自启（保存时同步注册表）
    pub autostart: bool,
    /// 全局热键（如 "Ctrl+Shift+K"），None/空串表示禁用；保存时重新注册
    pub hotkey: Option<String>,
    /// 界面语言："system" / "zh" / "en"；None 等同 "system"（跟随系统区域）
    pub language: Option<String>,
}

impl From<storage::Settings> for AppSettings {
    fn from(s: storage::Settings) -> Self {
        Self {
            login_method: s.login_method,
            refresh_interval_min: s.refresh_interval_min,
            low_warn_enabled: s.low_warn_enabled,
            warn_threshold_pct: s.warn_threshold_pct,
            autostart: s.autostart,
            hotkey: s.hotkey,
            language: s.language,
        }
    }
}

impl From<AppSettings> for storage::Settings {
    fn from(s: AppSettings) -> Self {
        Self {
            login_method: s.login_method,
            refresh_interval_min: s.refresh_interval_min,
            low_warn_enabled: s.low_warn_enabled,
            warn_threshold_pct: s.warn_threshold_pct,
            autostart: s.autostart,
            hotkey: s.hotkey,
            language: s.language,
        }
    }
}

/// 凭证配置状态（与 src/types.ts 的 CredentialStatus 一一对应）
#[derive(Debug, Clone, Serialize)]
pub struct CredentialStatus {
    /// 当前生效的登录方式（settings.login_method）
    pub login_method: Option<String>,
    pub api_key_configured: bool,
    /// 脱敏展示，如 sk-kimi-****…a4nr；未配置为 None
    pub api_key_masked: Option<String>,
    pub oauth_configured: bool,
    /// 网页 token（月度总量用）是否已配置
    pub web_token_configured: bool,
}

/// 设备码登录流程状态（与 src/types.ts 的 DeviceLoginState 一一对应，
/// 既是 start_device_login 的返回值，也是 device-login-updated 事件的 payload）
#[derive(Debug, Clone, Serialize)]
pub struct DeviceLoginState {
    /// idle=未开始/已取消，waiting=等待用户授权，success=已拿到 token，error=失败
    pub status: String,
    /// 展示给用户的授权码（waiting 时有值）
    pub user_code: Option<String>,
    pub verification_uri: Option<String>,
    /// 含码直达链接，前端"打开浏览器"用
    pub verification_uri_complete: Option<String>,
    /// 设备码有效期（秒）
    pub expires_in: Option<u64>,
    /// status=error 时的错误信息
    pub error: Option<String>,
}

impl Default for DeviceLoginState {
    fn default() -> Self {
        Self::idle()
    }
}

impl DeviceLoginState {
    fn idle() -> Self {
        Self {
            status: "idle".to_string(),
            user_code: None,
            verification_uri: None,
            verification_uri_complete: None,
            expires_in: None,
            error: None,
        }
    }

    fn waiting(info: &oauth::DeviceAuthInfo) -> Self {
        Self {
            status: "waiting".to_string(),
            user_code: Some(info.user_code.clone()),
            verification_uri: Some(info.verification_uri.clone()),
            verification_uri_complete: info.verification_uri_complete.clone(),
            expires_in: Some(info.expires_in),
            error: None,
        }
    }

    fn success() -> Self {
        Self {
            status: "success".to_string(),
            ..Self::idle()
        }
    }

    fn error(message: String) -> Self {
        Self {
            status: "error".to_string(),
            error: Some(message),
            ..Self::idle()
        }
    }
}

/// 更新检查结果（与 src/types.ts 的 UpdateInfo 一一对应，snake_case 序列化）。
/// 进程级时间缓存：上次成功 6 小时 / 上次错误 10 分钟内复用旧结果，
/// force 参数可强制走网络（见 check_update）
#[derive(Debug, Clone, Serialize)]
pub struct UpdateInfo {
    /// 当前版本，如 0.1.0
    pub current: String,
    /// 远端最新版本（检查失败为 None）
    pub latest: Option<String>,
    /// 是否有新版本
    pub has_update: bool,
    /// 有新版本时的 Release 页面地址（点击去下载）
    pub release_url: Option<String>,
    /// 检查失败原因（网络等）；成功为 None
    pub error: Option<String>,
}

/// 面板状态（与 src/types.ts 的 PanelState 一一对应，snake_case 序列化）
#[derive(Debug, Clone, Serialize)]
pub struct PanelState {
    /// 是否已配置任一凭证（API Key 或 OAuth）
    pub credential: bool,
    /// 是否正在后台刷新
    pub loading: bool,
    /// 最近一次成功的配额（可能来自缓存；断网时依然展示）
    pub quota: Option<KimiQuota>,
    /// 上次成功刷新时间（epoch 秒）
    pub fetched_at: Option<i64>,
    /// 最近一次错误信息（与缓存并存，用于非阻断横幅）
    pub error: Option<String>,
    /// 任一窗口剩余低于阈值，UI 标红
    pub low_warning: bool,
    /// 月度总量（已配置网页 token 且有数据时展示；可能为 None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monthly: Option<MonthlyInfo>,
    /// 月度数据获取失败原因（如网页登录态过期）；成功为 None
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monthly_error: Option<String>,
}

/// 进程内共享状态（`.manage(AppState::new())`，启动时从 cache.json 预热）
pub struct AppState {
    inner: Mutex<Inner>,
    /// 设备码登录流程状态与取消通道（锁内不做任何 await）
    device_login: Mutex<DeviceLoginInner>,
    /// 刷新单航班锁：并发 do_refresh 排队执行，杜绝 loading 快照泄漏卡死
    refresh_lock: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct Inner {
    loading: bool,
    error: Option<String>,
    /// 最近一次成功刷新（配额, epoch 秒）；启动时由 cache.json 预热
    last_quota: Option<(KimiQuota, i64)>,
    /// 最近一次成功的 usages 原始响应（超 20KB 截断）与时间戳；诊断导出用
    last_raw_response: Option<(String, i64)>,
    /// 最近一次成功的月度总量；启动时由 cache.json 预热
    monthly: Option<MonthlyInfo>,
    /// 月度数据获取失败原因（如网页登录态过期）；成功为 None
    monthly_error: Option<String>,
}

#[derive(Default)]
struct DeviceLoginInner {
    /// 当前登录流程状态（默认 idle）
    state: DeviceLoginState,
    /// 取消通道发送端（后台轮询任务持 receiver）；无进行中任务时为 None
    cancel: Option<watch::Sender<bool>>,
}

impl AppState {
    pub fn new() -> Self {
        let cache = storage::load_cache();
        let last_quota = cache.as_ref().map(|c| (c.quota.clone(), c.fetched_at));
        // 月度数据随配额缓存一并预热（断网/未刷新也能展示）
        let monthly = cache.and_then(|c| c.monthly);
        Self {
            inner: Mutex::new(Inner {
                loading: false,
                error: None,
                last_quota,
                last_raw_response: None,
                monthly,
                monthly_error: None,
            }),
            device_login: Mutex::new(DeviceLoginInner::default()),
            refresh_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// 当前内存态组装为 PanelState（settings 每次现读，设置页改阈值即刻生效）
    pub fn snapshot(&self) -> PanelState {
        let inner = self.inner.lock().unwrap();
        assemble_panel_state(&inner)
    }

    /// 最近一次成功的 usages 原始响应（已截断）与时间戳；诊断导出用
    pub fn raw_response(&self) -> Option<(String, i64)> {
        self.inner.lock().unwrap().last_raw_response.clone()
    }
}

/// 刷新主流程（轮询 / 托盘菜单"刷新" / 面板显示时共用）。
/// 单航班合并：并发调用排队等在途刷新完成，各自拿到完整新状态；
/// 全局兜底超时保证 loading 永远不会被永久置位。
pub async fn do_refresh(app: &AppHandle) -> PanelState {
    let state = app.state::<AppState>();

    let _permit = state.refresh_lock.lock().await;

    tracing::info!("开始刷新配额");
    {
        let mut inner = state.inner.lock().unwrap();
        inner.loading = true;
    }

    // 任何环节挂起（网络 / 系统凭证服务）都不能把 loading 永久置位
    let outcome =
        match tokio::time::timeout(std::time::Duration::from_secs(45), fetch_with_credential())
            .await
        {
            Ok(outcome) => outcome,
            Err(_) => FetchOutcome::Failed("请求超时，请检查网络".to_string()),
        };

    // 月度总量（网页 token）：拿到配额后无论成败都继续尝试
    let monthly_outcome = fetch_monthly().await;

    let panel = {
        let mut inner = state.inner.lock().unwrap();
        // 所有分支必须先复位 loading（NoCredential 曾漏掉这行导致永久卡死）
        inner.loading = false;
        let quota_success = matches!(outcome, FetchOutcome::Success(..));
        match outcome {
            // 当前登录方式无凭证：若另一种方式有凭证（用户刚切了方式），给引导文案；
            // 完全没有凭证则 error=None，面板显示 EmptyState 引导
            FetchOutcome::NoCredential => {
                inner.error = if has_any_credential() {
                    Some("当前登录方式未配置凭证，请到设置页配置或切换".to_string())
                } else {
                    None
                };
            }
            // 成功：清错误、更新 last_quota 与原始响应（缓存在下方统一落盘）
            FetchOutcome::Success(payload) => {
                let (quota, fetched_at, raw) = *payload;
                tracing::info!("配额已更新");
                inner.error = None;
                inner.last_raw_response = Some((truncate_raw_body(raw), fetched_at));
                inner.last_quota = Some((quota, fetched_at));
            }
            // 失败：保留旧缓存数据，仅记错误（错误类型与文案已在 fetch_with_credential 记 warn）
            FetchOutcome::Failed(message) => inner.error = Some(message),
        }

        // 月度结果：成功才覆盖数据；失败一律保留旧数据，仅记原因
        let mut monthly_success = false;
        match monthly_outcome {
            // 未配置网页 token：清空月度展示
            MonthlyOutcome::NoToken => {
                inner.monthly = None;
                inner.monthly_error = None;
            }
            MonthlyOutcome::Success(info) => {
                inner.monthly = Some(info);
                inner.monthly_error = None;
                monthly_success = true;
            }
            MonthlyOutcome::Unauthorized => {
                inner.monthly_error = Some("网页登录态已过期，请到设置页更新".to_string());
            }
            MonthlyOutcome::Failed => {
                inner.monthly_error = Some("月度数据刷新失败".to_string());
            }
        }

        // 配额或月度任一成功：把最新内存态落盘（月度挂在配额缓存上，向后兼容；
        // 配额失败但月度成功时沿用旧配额数据，fetched_at 不变）
        if quota_success || monthly_success {
            if let Some((quota, fetched_at)) = &inner.last_quota {
                let _ = storage::save_cache(&CachedQuota {
                    quota: quota.clone(),
                    fetched_at: *fetched_at,
                    monthly: inner.monthly.clone(),
                });
            }
        }

        // 配额刷新成功：追加一条本地历史采样（用量趋势图数据，纯事实不预测）。
        // 月度取本轮最终值（失败沿用旧值，未配置为 None）；t 用本轮成功时刻。
        // 历史是派生数据，读写失败只记日志，不影响刷新主流程
        if quota_success {
            if let Some((quota, fetched_at)) = &inner.last_quota {
                let mut store = history::HistoryStore::load();
                store.append(history::sample_point(
                    quota,
                    inner.monthly.as_ref(),
                    *fetched_at,
                ));
                if let Err(e) = store.save() {
                    tracing::warn!("保存用量历史失败: {e}");
                }
            }
        }

        assemble_panel_state(&inner)
    };

    // 更新托盘（图标 + tooltip 摘要）；失败时 quota 未变，属幂等重刷。
    // tooltip 文案语言随设置现读现解析，与 assemble_panel_state 的"设置现读"语义一致
    let lang = i18n::resolve(
        storage::load_settings()
            .unwrap_or_default()
            .language
            .as_deref(),
    );
    tray::update_tray_state(
        app,
        panel.low_warning,
        tooltip_extra(panel.quota.as_ref(), lang),
    );

    // 通知前端状态已变化
    let _ = app.emit("quota-updated", &panel);

    panel
}

/// 面板即将由隐藏变显示时调用：无缓存或数据陈旧（>60s）则后台刷新；
/// 距上次成功更新检查 ≥6 小时时顺带后台查一次更新
pub fn refresh_if_stale(app: &AppHandle) {
    let stale = {
        let state = app.state::<AppState>();
        let inner = state.inner.lock().unwrap();
        match inner.last_quota {
            Some((_, fetched_at)) => now_unix() - fetched_at > STALE_SECS,
            None => true,
        }
    };
    if stale {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            do_refresh(&app).await;
        });
    }
    check_update_if_stale(app);
}

/// 距上次成功更新检查 ≥6 小时则后台查一次，完成后广播 update-info 事件。
/// 上次是错误结果也视为到期（其 10 分钟防刷窗口由 check_update 自身缓存兜底）。
fn check_update_if_stale(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // 持锁期间不 await：只读缓存判断是否到期，网络检查在放锁后进行
        let due = {
            let cache = UPDATE_CACHE.lock().await;
            match &*cache {
                // 上次成功且未满 6 小时：不必查
                Some((info, at)) if info.error.is_none() => {
                    at.elapsed().as_secs() >= UPDATE_CACHE_OK_SECS
                }
                // 从未查过或上次是错误：到期
                _ => true,
            }
        };
        if !due {
            return;
        }
        let info = check_update(app.clone(), Some(false)).await;
        let _ = app.emit("update-info", &info);
    });
}

// 托盘 tooltip / 通知正文共用的窗口摘要已移至 crate::i18n::quota_summary（按语言出中英文案）

#[tauri::command]
pub fn get_panel_state(state: State<'_, AppState>) -> PanelState {
    state.snapshot()
}

/// 用量趋势历史（本地累积的成功刷新采样，纯事实不预测）：
/// load 后按 t 升序返回；无历史或文件损坏为空数组
#[tauri::command]
pub fn get_usage_history() -> Vec<history::HistoryPoint> {
    history::HistoryStore::load().into_points()
}

#[tauri::command]
pub async fn refresh_now(app: AppHandle) -> PanelState {
    do_refresh(&app).await
}

/// 检查应用更新。时间缓存策略（进程级静态缓存，见 UPDATE_CACHE）：
/// 上次成功 6 小时内、上次错误 10 分钟内（防刷）直接返回缓存；
/// force == Some(true)（设置页"检查更新"按钮）无条件走网络。
/// 新鲜结果会广播 update-info 事件，面板据此更新版本徽标。
#[tauri::command]
pub async fn check_update(app: AppHandle, force: Option<bool>) -> UpdateInfo {
    if force != Some(true) {
        // 持锁期间不 await：命中 TTL 即返回，未命中先放锁再走网络
        let cache = UPDATE_CACHE.lock().await;
        if let Some((info, at)) = &*cache {
            let ttl_secs = if info.error.is_none() {
                UPDATE_CACHE_OK_SECS
            } else {
                UPDATE_CACHE_ERR_SECS
            };
            if at.elapsed().as_secs() < ttl_secs {
                return info.clone();
            }
        }
    }
    // 并发未命中时各自打一次网络（旧 OnceCell 是单航班）：压力可忽略，
    // 成功 6h / 错误 10min 的缓存窗口已限频
    let info = fetch_update_info().await;
    *UPDATE_CACHE.lock().await = Some((info.clone(), std::time::Instant::now()));
    // 只广播新鲜结果：缓存命中不重复广播（面板挂载时本就会主动查一次）
    let _ = app.emit("update-info", &info);
    info
}

/// 进程级更新检查缓存：上次结果 + 完成时刻
static UPDATE_CACHE: tokio::sync::Mutex<Option<(UpdateInfo, std::time::Instant)>> =
    tokio::sync::Mutex::const_new(None);

/// 成功结果缓存时长（秒）：6 小时
const UPDATE_CACHE_OK_SECS: u64 = 6 * 3600;
/// 错误结果缓存时长（秒）：10 分钟，限流期防刷
const UPDATE_CACHE_ERR_SECS: u64 = 10 * 60;

/// 真正走网络的更新检查：拉取最新 Release（重定向路径优先，API 兜底），与内置版本号比较
async fn fetch_update_info() -> UpdateInfo {
    let current = env!("CARGO_PKG_VERSION").to_string();
    // UA / 10s 超时 / 不跟随重定向由 update::fetch_latest 统一配置
    match update::fetch_latest().await {
        Ok(release) => {
            // 剥掉 tag 的 v/V 前缀：前端展示统一为 "v{latest}"，避免 "vv0.1.2" 双前缀
            let latest = release.tag.trim_start_matches(['v', 'V']).to_string();
            let has_update = update::is_newer(&latest, &current);
            tracing::info!(
                "更新检查完成: current={current}, latest={latest}, has_update={has_update}"
            );
            UpdateInfo {
                has_update,
                latest: Some(latest),
                release_url: Some(release.url),
                current,
                error: None,
            }
        }
        // 错误结果同样进缓存（10 分钟 TTL），避免限流期反复打 GitHub
        Err(message) => {
            tracing::warn!("更新检查失败: {message}");
            UpdateInfo {
                current,
                latest: None,
                has_update: false,
                release_url: None,
                error: Some(message),
            }
        }
    }
}

#[tauri::command]
pub fn open_settings(app: AppHandle) {
    if let Some(window) = app.get_webview_window("settings") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[tauri::command]
pub fn get_settings() -> AppSettings {
    storage::load_settings().unwrap_or_default().into()
}

#[tauri::command]
pub fn save_settings(app: AppHandle, settings: AppSettings) -> Result<(), String> {
    let mut settings: storage::Settings = settings.into();
    // 钳制非法值（与 load_settings 的加载钳制语义一致）
    settings.refresh_interval_min = settings
        .refresh_interval_min
        .max(storage::MIN_REFRESH_INTERVAL_MIN);
    settings.warn_threshold_pct = settings.warn_threshold_pct.clamp(1.0, 99.0);
    storage::save_settings(&settings)?;

    // 同步开机自启注册表；与系统状态已一致时不操作，失败只记日志不阻断保存
    let autostart = app.autolaunch();
    match autostart.is_enabled() {
        Ok(enabled) if enabled != settings.autostart => {
            let result = if settings.autostart {
                autostart.enable()
            } else {
                autostart.disable()
            };
            match result {
                Ok(()) => tracing::info!("开机自启已同步: {}", settings.autostart),
                Err(e) => tracing::warn!("同步开机自启失败: {e}"),
            }
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("读取开机自启状态失败: {e}"),
    }

    // 保存成功后重注册全局热键（先全量注销再按新值注册）；
    // 被占用时返回中文错误（此时设置已落盘，仅热键未生效）
    crate::hotkey::apply(&app, settings.hotkey.as_deref())?;

    // 全部生效后广播 settings-changed（payload 为钳制后的完整设置），
    // 前端两窗口监听后即时切换语言等；热键失败走 ? 提前返回，不会广播半成品
    let _ = app.emit("settings-changed", AppSettings::from(settings));
    Ok(())
}

#[tauri::command]
pub fn set_api_key(key: String) -> Result<(), String> {
    let key = validate_api_key(&key)?;
    creds::save_api_key(key).map_err(|e| e.to_string())?;
    // 只记"已配置"，严禁记录 Key 本身
    tracing::info!("API Key 已配置");
    Ok(())
}

#[tauri::command]
pub fn clear_api_key() -> Result<(), String> {
    creds::clear_api_key().map_err(|e| e.to_string())?;
    tracing::info!("API Key 已清除");
    Ok(())
}

#[tauri::command]
pub fn get_credential_status() -> CredentialStatus {
    let settings = storage::load_settings().unwrap_or_default();
    let api_key = creds::load_api_key().ok().flatten();
    CredentialStatus {
        login_method: settings.login_method,
        api_key_configured: api_key.is_some(),
        api_key_masked: api_key.as_deref().map(mask_api_key),
        oauth_configured: matches!(oauth::load_credentials(), Ok(Some(_))),
        web_token_configured: matches!(creds::load_web_token(), Ok(Some(_))),
    }
}

/// 保存网页 token（kimi-auth）：先规范化，再真实调 GetSubscriptionStats 校验；
/// 通过后存凭据管理器并触发一次刷新，让月度数据立刻上面板
#[tauri::command]
pub async fn set_web_token(app: AppHandle, token: String) -> Result<MonthlyInfo, String> {
    let token = web::normalize_web_token(&token)?;
    match web::fetch_subscription_stats(&token).await {
        Ok(info) => {
            creds::save_web_token(&token).map_err(|e| e.to_string())?;
            // 只记"已配置"，严禁记录 token 本身
            tracing::info!("网页 token 已配置");
            do_refresh(&app).await;
            Ok(info)
        }
        Err(WebError::Unauthorized) => {
            Err("网页登录态无效或已过期，请重新复制 kimi-auth 的值".to_string())
        }
        Err(WebError::Http(_)) => Err("网络错误，校验失败".to_string()),
        // Parse：展示原始错误文本
        Err(e @ WebError::Parse(_)) => Err(e.to_string()),
    }
}

/// 清除网页 token 并触发一次刷新（面板回到无月度数据态）
#[tauri::command]
pub async fn clear_web_token(app: AppHandle) -> Result<(), String> {
    creds::clear_web_token().map_err(|e| e.to_string())?;
    tracing::info!("网页 token 已清除");
    do_refresh(&app).await;
    Ok(())
}

/// 发起设备码登录：先取设备授权码，立即返回 waiting 状态；
/// 后台任务轮询 token，结果经 device-login-updated 事件推送
#[tauri::command]
pub async fn start_device_login(app: AppHandle) -> DeviceLoginState {
    let info = match oauth::start_device_auth().await {
        Ok(info) => info,
        Err(e) => {
            tracing::warn!("设备码登录发起失败: {e}");
            return DeviceLoginState::error(e.to_string());
        }
    };
    tracing::info!("设备码登录已发起，等待用户授权");

    let waiting = DeviceLoginState::waiting(&info);
    let (tx, rx) = watch::channel(false);
    {
        let state = app.state::<AppState>();
        let mut dl = state.device_login.lock().unwrap();
        // 已有登录任务在跑：先发取消信号再顶替（旧任务收尾时发现 sender 已换，
        // 不会覆盖新状态，也不会多发事件）
        if let Some(old) = dl.cancel.take() {
            let _ = old.send(true);
        }
        dl.state = waiting.clone();
        dl.cancel = Some(tx.clone());
    }

    let app_task = app.clone();
    tauri::async_runtime::spawn(async move {
        run_device_login(app_task, info, rx, tx).await;
    });

    waiting
}

#[tauri::command]
pub fn cancel_device_login(app: AppHandle) {
    {
        let state = app.state::<AppState>();
        let mut dl = state.device_login.lock().unwrap();
        if let Some(tx) = dl.cancel.take() {
            // 通知后台轮询任务退出；任务侧发现 sender 已被取走，不会再发事件
            let _ = tx.send(true);
        }
        dl.state = DeviceLoginState::idle();
    }
    let _ = app.emit("device-login-updated", DeviceLoginState::idle());
}

#[tauri::command]
pub fn oauth_logout(app: AppHandle) {
    if let Err(e) = oauth::clear_credentials() {
        tracing::warn!("清除 OAuth 凭证失败: {e}");
    } else {
        tracing::info!("OAuth 凭证已清除");
    }
    // 当前以 oauth 登录：回退为未显式选择（自动优先 api_key）
    let mut settings = storage::load_settings().unwrap_or_default();
    if settings.login_method.as_deref() == Some("oauth") {
        settings.login_method = None;
        if let Err(e) = storage::save_settings(&settings) {
            tracing::warn!("保存登录方式失败: {e}");
        }
    }
    // 让面板回到无凭证态（do_refresh 对无凭证分支不发事件，这里手动组状态发）
    let panel = app.state::<AppState>().snapshot();
    let _ = app.emit("quota-updated", &panel);
}

/// 打开日志目录（确保存在后用系统文件管理器定位）
#[tauri::command]
pub fn open_log_dir(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let dir = crate::logging::log_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建日志目录失败: {e}"))?;
    app.opener()
        .reveal_item_in_dir(&dir)
        .map_err(|e| format!("打开日志目录失败: {e}"))?;
    tracing::info!("已打开日志目录");
    Ok(())
}

/// 导出诊断报告到配置目录并定位文件，返回诊断文件路径
#[tauri::command]
pub fn export_diagnostics(app: AppHandle) -> Result<String, String> {
    use tauri_plugin_opener::OpenerExt;
    let state = app.state::<AppState>();
    let path = crate::diagnostics::export(&state.snapshot(), state.raw_response())?;
    // 文件已写好；定位失败只记日志，不影响返回路径
    if let Err(e) = app.opener().reveal_item_in_dir(&path) {
        tracing::warn!("定位诊断文件失败: {e}");
    }
    tracing::info!("诊断报告已导出: {}", path.display());
    Ok(path.to_string_lossy().to_string())
}

// ---------------------------------------------------------------------------
// 以下为内部实现
// ---------------------------------------------------------------------------

/// Kimi Code API Key 前缀（与开放平台 sk- 不通用）
const API_KEY_PREFIX: &str = "sk-kimi-";
/// Key 校验失败时返回给前端的文案
const INVALID_API_KEY_MESSAGE: &str =
    "无效 Key：Kimi Code API Key 以 sk-kimi- 开头（与开放平台 sk- 不通用），请到 kimi.com/code/console 获取";

/// trim 后校验前缀，返回可用的 Key 切片
fn validate_api_key(key: &str) -> Result<&str, String> {
    let key = key.trim();
    if key.starts_with(API_KEY_PREFIX) {
        Ok(key)
    } else {
        Err(INVALID_API_KEY_MESSAGE.to_string())
    }
}

/// API Key 脱敏：长度 > 12 显示 前 8 字符 + "…" + 后 4 字符，否则全显
fn mask_api_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() > 12 {
        let prefix: String = chars[..8].iter().collect();
        let suffix: String = chars[chars.len() - 4..].iter().collect();
        format!("{prefix}…{suffix}")
    } else {
        key.to_string()
    }
}

/// 设备码登录后台任务：轮询 token 与取消信号竞争，收尾经 device-login-updated 推送。
/// cancel_tx 用于判断自己是否仍是"当前"登录流程（被顶替/取消时不覆盖新状态）。
async fn run_device_login(
    app: AppHandle,
    info: oauth::DeviceAuthInfo,
    mut cancel_rx: watch::Receiver<bool>,
    cancel_tx: watch::Sender<bool>,
) {
    let outcome = tokio::select! {
        result = oauth::poll_device_token(&info) => match result {
            Ok(credentials) => {
                match oauth::save_credentials(&credentials) {
                    Ok(()) => {
                        // 登录方式切到 oauth 并落盘，数据链路随即生效
                        let mut settings = storage::load_settings().unwrap_or_default();
                        settings.login_method = Some("oauth".to_string());
                        if let Err(e) = storage::save_settings(&settings) {
                            tracing::warn!("保存登录方式失败: {e}");
                        }
                        DeviceLoginState::success()
                    }
                    Err(e) => DeviceLoginState::error(format!("保存凭证失败: {e}")),
                }
            }
            // Expired / Denied / Api / Http：文案直接取自 OAuthError 的 Display
            Err(e) => DeviceLoginState::error(e.to_string()),
        },
        _ = cancel_rx.changed() => DeviceLoginState::idle(),
    };

    // 设备码登录结果（成功/失败/取消），不记任何 token
    match outcome.status.as_str() {
        "success" => tracing::info!("设备码登录成功"),
        "error" => tracing::warn!(
            "设备码登录失败: {}",
            outcome.error.as_deref().unwrap_or("未知错误")
        ),
        _ => tracing::info!("设备码登录已取消"),
    }

    // 只有仍是当前登录流程才更新状态并发事件：
    // 取消（sender 已取走）或被新流程顶替（sender 已换）时静默退出
    let is_current = {
        let state = app.state::<AppState>();
        let mut dl = state.device_login.lock().unwrap();
        if dl
            .cancel
            .as_ref()
            .is_some_and(|tx| cancel_tx.same_channel(tx))
        {
            dl.cancel = None;
            dl.state = outcome.clone();
            true
        } else {
            false
        }
    };
    if !is_current {
        return;
    }
    let _ = app.emit("device-login-updated", &outcome);

    // 授权成功：后台刷一次面板，让配额立刻用新凭证展示
    if outcome.status == "success" {
        let app_refresh = app.clone();
        tauri::async_runtime::spawn(async move {
            do_refresh(&app_refresh).await;
        });
    }
}

/// 单次刷新的三种结局
enum FetchOutcome {
    /// 未配置任何凭证
    NoCredential,
    /// 拉取成功（配额, epoch 秒, usages 原始响应）；装箱避免撑大枚举
    Success(Box<(KimiQuota, i64, String)>),
    /// 拉取失败（面向用户的中文错误）
    Failed(String),
}

/// 月度刷新的四种结局（独立于配额刷新结果处理）
enum MonthlyOutcome {
    /// 未配置网页 token
    NoToken,
    /// 拉取成功
    Success(MonthlyInfo),
    /// 网页登录态失效（401/403）：保留旧数据，提示更新
    Unauthorized,
    /// 其他失败（网络/解析/keyring）：保留旧数据
    Failed,
}

/// 取网页 token 并拉取月度总量；未配置 token → NoToken
async fn fetch_monthly() -> MonthlyOutcome {
    let token = match creds::load_web_token() {
        Ok(Some(token)) => token,
        Ok(None) => return MonthlyOutcome::NoToken,
        Err(_) => return MonthlyOutcome::Failed,
    };
    match web::fetch_subscription_stats(&token).await {
        Ok(info) => MonthlyOutcome::Success(info),
        Err(WebError::Unauthorized) => MonthlyOutcome::Unauthorized,
        Err(_) => MonthlyOutcome::Failed,
    }
}

/// 取 token 并拉取配额；失败分支记 warn（错误类型与文案，严禁记录 token）
async fn fetch_with_credential() -> FetchOutcome {
    let token = match creds::get_active_token().await {
        Ok(Some((_, token))) => token,
        Ok(None) => return FetchOutcome::NoCredential,
        Err(e) => {
            tracing::warn!("配额刷新失败: 读取凭证失败: {e}");
            return FetchOutcome::Failed(e.to_string());
        }
    };
    match KimiClient::new().fetch_quota_with_raw(&token).await {
        Ok((quota, raw)) => FetchOutcome::Success(Box::new((quota, now_unix(), raw))),
        Err(QuotaError::Unauthorized) => {
            tracing::warn!("配额刷新失败: 凭证无效或已过期 (Unauthorized)");
            FetchOutcome::Failed("凭证无效或已过期，请在设置中重新配置".to_string())
        }
        Err(QuotaError::Http(e)) => {
            tracing::warn!("配额刷新失败: 网络错误: {e}");
            FetchOutcome::Failed("网络错误，展示缓存数据".to_string())
        }
        // Parse / Api：展示原始错误文本
        Err(other) => {
            tracing::warn!("配额刷新失败: {other}");
            FetchOutcome::Failed(other.to_string())
        }
    }
}

/// usages 原始响应内存截断：按 char 边界截到 20KB 内（诊断导出用）
fn truncate_raw_body(mut body: String) -> String {
    const MAX: usize = crate::diagnostics::MAX_RAW_BODY_LEN;
    if body.len() > MAX {
        let mut end = MAX;
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        body.truncate(end);
    }
    body
}

/// 由内存态 + 当前设置/凭证组装 PanelState
fn assemble_panel_state(inner: &Inner) -> PanelState {
    let settings = storage::load_settings().unwrap_or_default();
    let (quota, fetched_at) = match &inner.last_quota {
        Some((quota, fetched_at)) => (Some(quota.clone()), Some(*fetched_at)),
        None => (None, None),
    };
    // 低额判定：任一时间窗剩余低于阈值，或月度已用超过 100 - 阈值
    let low_warning = quota
        .as_ref()
        .is_some_and(|q| needs_low_warning(q, settings.warn_threshold_pct))
        || inner
            .monthly
            .as_ref()
            .is_some_and(|m| m.total_pct >= 100.0 - settings.warn_threshold_pct);
    PanelState {
        credential: has_any_credential(),
        loading: inner.loading,
        quota,
        fetched_at,
        error: inner.error.clone(),
        low_warning,
        monthly: inner.monthly.clone(),
        monthly_error: inner.monthly_error.clone(),
    }
}

/// 是否已配置任一凭证（API Key 或 OAuth 本地凭证）
fn has_any_credential() -> bool {
    if matches!(creds::load_api_key(), Ok(Some(_))) {
        return true;
    }
    matches!(oauth::load_credentials(), Ok(Some(_)))
}

/// 托盘 tooltip 的附加行："\n7天剩余 87% · 5h剩余 36%"（英文 "\n7D left 87% · 5H left 36%"，
/// 无数据时为 None）；摘要文案按 lang 由 i18n::quota_summary 生成
fn tooltip_extra(quota: Option<&KimiQuota>, lang: i18n::Lang) -> Option<String> {
    let summary = quota.map(|q| i18n::quota_summary(lang, q))?;
    if summary.is_empty() {
        None
    } else {
        Some(format!("\n{summary}"))
    }
}

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- API Key 校验 ----

    #[test]
    fn validate_api_key_accepts_kimi_prefix() {
        assert_eq!(
            validate_api_key("sk-kimi-abc123").unwrap(),
            "sk-kimi-abc123"
        );
    }

    #[test]
    fn validate_api_key_trims_surrounding_whitespace() {
        assert_eq!(
            validate_api_key("  sk-kimi-abc123 \n").unwrap(),
            "sk-kimi-abc123"
        );
    }

    #[test]
    fn validate_api_key_rejects_open_platform_key() {
        // 开放平台 sk- 开头的 Key 与 Kimi Code 不通用
        let err = validate_api_key("sk-abcdef123456").unwrap_err();
        assert_eq!(err, INVALID_API_KEY_MESSAGE);
    }

    #[test]
    fn validate_api_key_rejects_empty_or_blank() {
        assert!(validate_api_key("").is_err());
        assert!(validate_api_key("   ").is_err());
    }

    // ---- API Key 脱敏 ----

    #[test]
    fn mask_api_key_long_key_shows_head_and_tail() {
        // 长度 > 12：前 8 + "…" + 后 4
        assert_eq!(mask_api_key("sk-kimi-abcdefgh1234"), "sk-kimi-…1234");
    }

    #[test]
    fn mask_api_key_exactly_12_chars_fully_shown() {
        assert_eq!(mask_api_key("sk-kimi-ab12"), "sk-kimi-ab12");
    }

    #[test]
    fn mask_api_key_13_chars_masks() {
        assert_eq!(mask_api_key("sk-kimi-abcde"), "sk-kimi-…bcde");
    }

    #[test]
    fn mask_api_key_empty() {
        assert_eq!(mask_api_key(""), "");
    }
}
