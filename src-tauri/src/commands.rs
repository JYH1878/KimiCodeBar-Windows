//! Tauri 命令层与刷新编排（bin 侧）：面板状态组装、防重入刷新、托盘/事件联动。
//!
//! 多账号：一轮 `do_refresh` 遍历全部账号（单航班锁保护整轮），各账号独立存
//! 配额/月度/错误快照；`quota-updated` 事件 payload 为含 accounts 数组的 PanelState。
//!
//! 前端契约（`src/types.ts`）：`get_panel_state()` / `refresh_now()` → PanelState。

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Mutex;

use kimicodebar::creds;
use kimicodebar::deepseek::client::DeepSeekClient;
use kimicodebar::deepseek::models::DeepSeekBalance;
use kimicodebar::glm::client::GlmClient;
use kimicodebar::history;
use kimicodebar::kimi::client::KimiClient;
use kimicodebar::kimi::oauth;
use kimicodebar::kimi::web::{self, MonthlyInfo, WebError};
use kimicodebar::quota::{deepseek_needs_low_warning, needs_low_warning, KimiQuota, QuotaError};
use kimicodebar::storage::{self, Account, CachedQuota};
use kimicodebar::update;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_autostart::ManagerExt;
use tokio::sync::watch;

use crate::i18n;
use crate::tray;

/// 面板距上次成功刷新超过该秒数，再次显示时触发后台刷新
const STALE_SECS: i64 = 60;

/// 应用设置（与 src/types.ts 的 AppSettings 一一对应，snake_case；Deserialize 用于收参）。
/// 注意：账号列表与登录方式不在此（属 Account / settings.json 的 accounts 数组，
/// 由 list_accounts / add_account 等账号命令管理）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    /// 自动刷新间隔（分钟，1–60，默认 5）
    pub refresh_interval_min: u32,
    /// 刷新模式：true=自适应（活跃时 1 分钟，静默按固定间隔），默认 true
    pub adaptive_refresh: bool,
    /// 低额度告警开关
    pub low_warn_enabled: bool,
    /// 告警阈值（剩余百分比，1–99）
    pub warn_threshold_pct: f64,
    /// DeepSeek 低余额告警阈值（元，0–100000，默认 5）
    pub deepseek_warn_threshold: f64,
    /// 开机自启（保存时同步注册表）
    pub autostart: bool,
    /// 极简模式：开后面板只显示 7 天 / 5 小时额度条（窗口压矮），默认关
    pub minimal_mode: bool,
    /// 全局热键（如 "Ctrl+Shift+K"），None/空串表示禁用；保存时重新注册
    pub hotkey: Option<String>,
    /// 界面语言："system" / "zh" / "en"；None 等同 "system"（跟随系统区域）
    pub language: Option<String>,
    /// 主题模式："system" / "dark" / "light"；None 等同 "system"（跟随系统明暗）
    /// （types.ts 标为可选，反序列化容忍缺省）
    #[serde(default)]
    pub theme: Option<String>,
    /// 面板背景图片文件名（存于配置目录），None 表示无自定义背景
    #[serde(default)]
    pub background_image: Option<String>,
    /// 预设背景 id（night / aurora / violet / ember），None 表示未选；生效时优先于 image
    #[serde(default)]
    pub background_preset: Option<String>,
}

impl From<storage::Settings> for AppSettings {
    fn from(s: storage::Settings) -> Self {
        Self {
            refresh_interval_min: s.refresh_interval_min,
            adaptive_refresh: s.adaptive_refresh,
            low_warn_enabled: s.low_warn_enabled,
            warn_threshold_pct: s.warn_threshold_pct,
            deepseek_warn_threshold: s.deepseek_warn_threshold,
            autostart: s.autostart,
            minimal_mode: s.minimal_mode,
            hotkey: s.hotkey,
            language: s.language,
            theme: s.theme,
            background_image: s.background_image,
            background_preset: s.background_preset,
        }
    }
}

impl From<AppSettings> for storage::Settings {
    fn from(s: AppSettings) -> Self {
        Self {
            // 账号列表与废弃的全局 login_method 由调用方补（save_settings 先读后写）
            accounts: Vec::new(),
            login_method: None,
            refresh_interval_min: s.refresh_interval_min,
            adaptive_refresh: s.adaptive_refresh,
            low_warn_enabled: s.low_warn_enabled,
            warn_threshold_pct: s.warn_threshold_pct,
            deepseek_warn_threshold: s.deepseek_warn_threshold,
            autostart: s.autostart,
            minimal_mode: s.minimal_mode,
            hotkey: s.hotkey,
            language: s.language,
            theme: s.theme,
            background_image: s.background_image,
            background_preset: s.background_preset,
        }
    }
}

/// 凭证配置状态（与 src/types.ts 的 CredentialStatus 一一对应；按账号查询）
#[derive(Debug, Clone, Serialize)]
pub struct CredentialStatus {
    /// 该账号当前生效的登录方式（account.login_method）
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

/// 单个账号的面板快照（PanelState.accounts 的元素，与 src/types.ts 的 AccountPanel 对应）
#[derive(Debug, Clone, Serialize)]
pub struct AccountPanel {
    /// 账号元数据（id / name / login_method）
    pub account: Account,
    /// 该账号是否已配置任一凭证（API Key 或 OAuth）
    pub credential: bool,
    /// 最近一次成功的配额（可能来自缓存；断网时依然展示）
    pub quota: Option<KimiQuota>,
    /// 上次成功刷新时间（epoch 秒）
    pub fetched_at: Option<i64>,
    /// 最近一次错误信息（与缓存并存，用于非阻断横幅）
    pub error: Option<String>,
    /// 任一窗口剩余低于阈值，UI 标红（最近刷新失败/凭证无效时恒为 false，GOAL 拍板）
    pub low_warning: bool,
    /// 月度总量（已配置网页 token 且有数据时展示；可能为 None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monthly: Option<MonthlyInfo>,
    /// 月度数据获取失败原因（如网页登录态过期）；成功为 None
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monthly_error: Option<String>,
    /// DeepSeek 余额（仅 provider=deepseek 的账号有值；Kimi 账号恒为 None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deepseek_balance: Option<DeepSeekBalance>,
}

/// 面板状态（与 src/types.ts 的 PanelState 一一对应，snake_case 序列化）
#[derive(Debug, Clone, Serialize)]
pub struct PanelState {
    /// 是否正在后台刷新（整轮全部账号，单航班）
    pub loading: bool,
    /// 各账号快照（顺序 = settings.accounts 顺序 = 面板页顺序）
    pub accounts: Vec<AccountPanel>,
}

/// 进程内共享状态（`.manage(AppState::new())`，启动时从各账号 cache-<id>.json 预热）
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
    /// 每账号运行时快照（按账号 id 索引；顺序无关，展示顺序由 settings.accounts 决定）
    accounts: HashMap<String, AccountRuntime>,
}

/// 单账号的运行时快照（启动时由 cache-<id>.json 预热）
#[derive(Default, Clone)]
struct AccountRuntime {
    /// 最近一次错误信息（配额刷新失败；成功为 None）
    error: Option<String>,
    /// 最近一次成功刷新（配额, epoch 秒）
    last_quota: Option<(KimiQuota, i64)>,
    /// 最近一次成功的 usages 原始响应（超 20KB 截断）与时间戳；诊断导出用
    last_raw_response: Option<(String, i64)>,
    /// 最近一次成功的月度总量
    monthly: Option<MonthlyInfo>,
    /// 月度数据获取失败原因（如网页登录态过期）；成功为 None
    monthly_error: Option<String>,
    /// 最近一次成功的 DeepSeek 余额（仅 provider=deepseek 的账号）
    last_balance: Option<(DeepSeekBalance, i64)>,
}

#[derive(Default)]
struct DeviceLoginInner {
    /// 当前登录流程状态（默认 idle）
    state: DeviceLoginState,
    /// 取消通道发送端（后台轮询任务持 receiver）；无进行中任务时为 None
    cancel: Option<watch::Sender<bool>>,
    /// 本流程绑定的账号 id（成功时凭证写入该账号；同一时间只跑一个流程）
    account_id: Option<String>,
}

impl AppState {
    pub fn new() -> Self {
        // 启动预热：每个账号各自的 cache-<id>.json（断网/未刷新也能展示）
        let settings = storage::load_settings().unwrap_or_default();
        let mut accounts = HashMap::new();
        for account in &settings.accounts {
            let cache = storage::load_cache(&account.id);
            let runtime = AccountRuntime {
                last_quota: cache.as_ref().map(|c| (c.quota.clone(), c.fetched_at)),
                // 月度数据随配额缓存一并预热
                monthly: cache.as_ref().and_then(|c| c.monthly.clone()),
                // DeepSeek 余额同缓存预热（断网/未刷新也能展示）
                last_balance: cache
                    .as_ref()
                    .and_then(|c| c.deepseek_balance.clone().map(|b| (b, c.fetched_at))),
                ..AccountRuntime::default()
            };
            accounts.insert(account.id.clone(), runtime);
        }
        Self {
            inner: Mutex::new(Inner {
                loading: false,
                accounts,
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

    /// 删除账号后收敛内存态：移除该账号的运行时快照（无残留）
    pub fn remove_account_runtime(&self, account_id: &str) {
        self.inner.lock().unwrap().accounts.remove(account_id);
    }

    /// 各账号最近一次成功的 usages 原始响应（已截断）：(账号名, 响应, epoch 秒)，诊断导出用
    pub fn raw_responses(&self) -> Vec<(String, String, i64)> {
        let inner = self.inner.lock().unwrap();
        let settings = storage::load_settings().unwrap_or_default();
        inner
            .accounts
            .iter()
            .filter_map(|(id, rt)| {
                rt.last_raw_response.clone().map(|(raw, ts)| {
                    let name = settings
                        .account(id)
                        .map(|a| a.name.clone())
                        .unwrap_or_else(|| id.clone());
                    (name, raw, ts)
                })
            })
            .collect()
    }
}

/// 刷新主流程（轮询 / 托盘菜单"刷新" / 面板显示时共用）：一轮刷新全部账号。
/// 单航班合并：并发调用排队等在途刷新完成，各自拿到完整新状态；
/// 每个账号 45s 兜底超时，保证 loading 永远不会被永久置位。
pub async fn do_refresh(app: &AppHandle) -> PanelState {
    let state = app.state::<AppState>();

    let _permit = state.refresh_lock.lock().await;

    let accounts = storage::load_settings().unwrap_or_default().accounts;
    tracing::info!("开始刷新配额（{} 个账号）", accounts.len());
    {
        let mut inner = state.inner.lock().unwrap();
        inner.loading = true;
    }

    // 逐账号拉取（配额 + 月度）：任何环节挂起都不能把 loading 永久置位
    let mut outcomes: Vec<(String, FetchOutcome, MonthlyOutcome)> = Vec::new();
    for account in &accounts {
        // 任何环节挂起（网络 / 系统凭证服务）都不能把 loading 永久置位
        let outcome = match tokio::time::timeout(
            std::time::Duration::from_secs(45),
            fetch_with_credential(account),
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(_) => FetchOutcome::Failed("请求超时，请检查网络".to_string()),
        };
        // 月度总量（网页 token）：拿到配额后无论成败都继续尝试；
        // 非 Kimi 账号（DeepSeek / GLM）无月度概念，直接跳过（GOAL 拍板）
        let monthly_outcome = if account.provider != "kimi" {
            MonthlyOutcome::NoToken
        } else {
            fetch_monthly(&account.id).await
        };
        outcomes.push((account.id.clone(), outcome, monthly_outcome));
    }

    let mut any_quota_success = false;
    let panel = {
        let mut inner = state.inner.lock().unwrap();
        // 所有分支必须先复位 loading（NoCredential 曾漏掉这行导致永久卡死）
        inner.loading = false;
        for (account_id, outcome, monthly_outcome) in outcomes {
            let runtime = inner.accounts.entry(account_id.clone()).or_default();
            let quota_success = matches!(outcome, FetchOutcome::Success(..));
            let balance_success = matches!(outcome, FetchOutcome::DeepSeekSuccess(..));
            match outcome {
                // 当前登录方式无凭证：若另一种方式有凭证（用户刚切了方式），给引导文案；
                // 完全没有凭证则 error=None，面板该页显示未配置引导
                FetchOutcome::NoCredential => {
                    runtime.error = if has_any_credential(&account_id) {
                        Some("当前登录方式未配置凭证，请到设置页配置或切换".to_string())
                    } else {
                        None
                    };
                }
                // 成功：清错误、更新 last_quota 与原始响应（缓存在下方统一落盘）
                FetchOutcome::Success(payload) => {
                    let (quota, fetched_at, raw) = *payload;
                    runtime.error = None;
                    runtime.last_raw_response = Some((truncate_raw_body(raw), fetched_at));
                    runtime.last_quota = Some((quota, fetched_at));
                }
                // DeepSeek 余额成功：清错误、更新 last_balance（无原始响应/配额概念）
                FetchOutcome::DeepSeekSuccess(payload) => {
                    let (balance, fetched_at) = *payload;
                    runtime.error = None;
                    runtime.last_balance = Some((balance, fetched_at));
                }
                // 失败：保留旧缓存数据，仅记错误（错误类型与文案已在 fetch_with_credential 记 warn）
                FetchOutcome::Failed(message) => runtime.error = Some(message),
            }

            // 月度结果：成功才覆盖数据；失败一律保留旧数据，仅记原因
            let mut monthly_success = false;
            match monthly_outcome {
                // 未配置网页 token：清空月度展示
                MonthlyOutcome::NoToken => {
                    runtime.monthly = None;
                    runtime.monthly_error = None;
                }
                MonthlyOutcome::Success(info) => {
                    runtime.monthly = Some(info);
                    runtime.monthly_error = None;
                    monthly_success = true;
                }
                MonthlyOutcome::Unauthorized => {
                    runtime.monthly_error = Some("网页登录态已过期，请到设置页更新".to_string());
                }
                MonthlyOutcome::Failed => {
                    runtime.monthly_error = Some("月度数据刷新失败".to_string());
                }
            }

            // 配额或月度或余额任一成功：把最新内存态落盘到该账号的 cache-<id>.json
            // （月度/余额挂在配额缓存上；配额失败但月度成功时沿用旧配额数据，fetched_at 不变；
            //   DeepSeek 账号无配额，以空配额占位、余额落盘供断网显示）
            if quota_success || monthly_success || balance_success {
                let fetched_at = runtime
                    .last_quota
                    .as_ref()
                    .map(|(_, t)| *t)
                    .or_else(|| runtime.last_balance.as_ref().map(|(_, t)| *t));
                if let Some(fetched_at) = fetched_at {
                    let _ = storage::save_cache(
                        &account_id,
                        &CachedQuota {
                            quota: runtime
                                .last_quota
                                .clone()
                                .map(|(q, _)| q)
                                .unwrap_or_default(),
                            fetched_at,
                            monthly: runtime.monthly.clone(),
                            deepseek_balance: runtime.last_balance.clone().map(|(b, _)| b),
                        },
                    );
                }
            }

            // 配额刷新成功：给该账号追加一条历史采样（用量趋势图数据，纯事实不预测）。
            // DeepSeek 账号不写历史（GOAL 拍板，本次无趋势图）；GLM 账号复用 Success 结局，
            // 与 Kimi 一样写历史。
            // 月度取本轮最终值（失败沿用旧值，未配置为 None）；t 用本轮成功时刻。
            // 历史是派生数据，读写失败只记日志，不影响刷新主流程
            if quota_success {
                any_quota_success = true;
                if let Some((quota, fetched_at)) = &runtime.last_quota {
                    let mut store = history::HistoryStore::load(&account_id);
                    store.append(history::sample_point(
                        quota,
                        runtime.monthly.as_ref(),
                        *fetched_at,
                    ));
                    if let Err(e) = store.save(&account_id) {
                        tracing::warn!("保存用量历史失败: {e}");
                    }
                }
            }
        }

        if any_quota_success {
            tracing::info!("配额已更新");
            // 顺手增量扫一次本地 token 统计（扫描自带 180s 节流与增量续读，
            // 开销可忽略；派生数据，失败不影响刷新主流程）
            tauri::async_runtime::spawn(async move {
                let _ = tokio::task::spawn_blocking(kimicodebar::local_usage::scan).await;
            });
        }

        assemble_panel_state(&inner)
    };

    // 更新托盘（图标 + tooltip 摘要）：任一账号低额即变红，tooltip 取最差账号摘要。
    // tooltip 文案语言随设置现读现解析，与 assemble_panel_state 的"设置现读"语义一致
    let lang = i18n::resolve(
        storage::load_settings()
            .unwrap_or_default()
            .language
            .as_deref(),
    );
    tray::update_tray_state(
        app,
        any_low_warning(&panel),
        worst_account_tooltip(&panel, lang),
    );

    // 通知前端状态已变化
    let _ = app.emit("quota-updated", &panel);

    panel
}

/// 面板即将由隐藏变显示时调用：任一账号无缓存或数据陈旧（>60s）则后台刷新；
/// 距上次成功更新检查 ≥6 小时时顺带后台查一次更新
pub fn refresh_if_stale(app: &AppHandle) {
    let stale = {
        let state = app.state::<AppState>();
        let inner = state.inner.lock().unwrap();
        let settings = storage::load_settings().unwrap_or_default();
        !settings.accounts.is_empty()
            && settings.accounts.iter().any(|account| {
                // Kimi 看配额时间，DeepSeek 看余额时间；两者皆无视为无缓存
                let fetched_at = inner.accounts.get(&account.id).and_then(|rt| {
                    rt.last_quota
                        .as_ref()
                        .map(|(_, t)| *t)
                        .or_else(|| rt.last_balance.as_ref().map(|(_, t)| *t))
                });
                match fetched_at {
                    Some(fetched_at) => now_unix() - fetched_at > STALE_SECS,
                    None => true,
                }
            })
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

#[tauri::command]
pub fn get_panel_state(state: State<'_, AppState>) -> PanelState {
    state.snapshot()
}

/// 用量趋势历史（按账号；本地累积的成功刷新采样，纯事实不预测）：
/// load 后按 t 升序返回；无历史或文件损坏为空数组
#[tauri::command]
pub fn get_usage_history(account_id: String) -> Vec<history::HistoryPoint> {
    history::HistoryStore::load(&account_id).into_points()
}

/// 本地 token 消耗统计（按账号归属：扫描 wire.jsonl 按 CLI 凭证快照归到各账号，不依赖 API）：
/// 取该账号的桶；无桶（从未归属到消耗）给默认空统计，last_scan_at 照填。
/// 增量扫描 + 180s 节流在 local_usage::scan 内部生效
#[tauri::command]
pub async fn get_local_usage(account_id: String) -> kimicodebar::local_usage::LocalUsageStats {
    // 首次全扫可能读几十 MB jsonl，放阻塞线程池，不占 async runtime worker
    tokio::task::spawn_blocking(move || kimicodebar::local_usage::scan().for_account(&account_id))
        .await
        .unwrap_or_default()
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

/// 打开设置窗口；section 指定定位分区（如 "account-add" 定位到账号添加表单），
/// 经 settings-navigate 事件通知设置页（设置窗自启动常驻，JS 监听始终在线）
#[tauri::command]
pub fn open_settings(app: AppHandle, section: Option<String>) {
    if let Some(window) = app.get_webview_window("settings") {
        let _ = window.show();
        let _ = window.set_focus();
    }
    if let Some(section) = section {
        let _ = app.emit("settings-navigate", section);
    }
}

#[tauri::command]
pub fn get_settings() -> AppSettings {
    storage::load_settings().unwrap_or_default().into()
}

#[tauri::command]
pub fn save_settings(app: AppHandle, settings: AppSettings) -> Result<(), String> {
    // 先读后写：AppSettings 不含账号列表，必须保留磁盘上的 accounts
    let mut settings: storage::Settings = {
        let current = storage::load_settings().unwrap_or_default();
        storage::Settings {
            accounts: current.accounts,
            ..settings.into()
        }
    };
    // 钳制非法值（与 load_settings 的加载钳制语义一致）
    settings.refresh_interval_min = settings.refresh_interval_min.clamp(
        storage::MIN_REFRESH_INTERVAL_MIN,
        storage::MAX_REFRESH_INTERVAL_MIN,
    );
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
    // 面板正开着时即时重算尺寸并重定位（极简模式开关压矮/恢复窗口）
    crate::panel::refit_open_panel(&app);
    Ok(())
}

/// 设置页录制热键前调用：临时注销所有全局热键，
/// 否则已注册的组合被系统全局拦截，录制输入框收不到按键
#[tauri::command]
pub fn pause_global_hotkey(app: AppHandle) {
    crate::hotkey::pause(&app);
}

/// 录制结束（成功/取消/失焦）调用：按已保存设置重新注册；
/// 恢复失败只记日志——下次保存设置时会再次尝试
#[tauri::command]
pub fn resume_global_hotkey(app: AppHandle) {
    if let Err(e) = crate::hotkey::resume(&app) {
        tracing::warn!("恢复全局热键失败: {e}");
    }
}

/// 保存面板背景图片（base64 静态图）：后端嗅探校验格式与大小后写入配置目录，
/// 成功后广播 settings-changed，面板即时换背景
#[tauri::command]
pub fn set_background_image(app: AppHandle, data_base64: String) -> Result<(), String> {
    kimicodebar::background::set_base64(&data_base64)?;
    let settings = storage::load_settings().unwrap_or_default();
    let _ = app.emit("settings-changed", AppSettings::from(settings));
    Ok(())
}

/// 清除面板背景图片并广播 settings-changed（未设置过为空操作）
#[tauri::command]
pub fn clear_background_image(app: AppHandle) -> Result<(), String> {
    kimicodebar::background::clear()?;
    let settings = storage::load_settings().unwrap_or_default();
    let _ = app.emit("settings-changed", AppSettings::from(settings));
    Ok(())
}

/// 选择预设背景（preset 为 None 表示取消预设，切回自定义图/无背景），
/// 后端白名单校验后落盘并广播 settings-changed，面板即时换背景
#[tauri::command]
pub fn set_background_preset(app: AppHandle, preset: Option<String>) -> Result<(), String> {
    kimicodebar::background::set_preset(preset.as_deref())?;
    let settings = storage::load_settings().unwrap_or_default();
    let _ = app.emit("settings-changed", AppSettings::from(settings));
    Ok(())
}

// ---------------------------------------------------------------------------
// 账号管理命令
// ---------------------------------------------------------------------------

/// 账号列表（顺序 = 面板页顺序）
#[tauri::command]
pub fn list_accounts() -> Vec<Account> {
    storage::load_settings().unwrap_or_default().accounts
}

/// 新增账号（全部提供商合计超上限 10 个报错；名称为空默认「账号 N」），返回新建账号。
/// provider 仅识别 "deepseek" / "glm"，其余（含缺省）按 "kimi"
#[tauri::command]
pub fn add_account(
    app: AppHandle,
    name: Option<String>,
    provider: Option<String>,
) -> Result<Account, String> {
    let mut settings = storage::load_settings().unwrap_or_default();
    let account = settings.add_account(name.as_deref(), provider.as_deref().unwrap_or("kimi"))?;
    storage::save_settings(&settings)?;
    tracing::info!(
        "账号已添加: {}（{}，{}）",
        account.name,
        account.id,
        account.provider
    );
    emit_snapshot(&app);
    Ok(account)
}

/// 账号改名（空名 / 不存在报错）
#[tauri::command]
pub fn rename_account(app: AppHandle, account_id: String, name: String) -> Result<(), String> {
    let mut settings = storage::load_settings().unwrap_or_default();
    settings.rename_account(&account_id, &name)?;
    storage::save_settings(&settings)?;
    tracing::info!("账号已改名: {account_id}");
    emit_snapshot(&app);
    Ok(())
}

/// 账号上移/下移（direction 取符号；越界为无操作）
#[tauri::command]
pub fn move_account(app: AppHandle, account_id: String, direction: i32) -> Result<(), String> {
    let mut settings = storage::load_settings().unwrap_or_default();
    if settings.move_account(&account_id, direction) {
        storage::save_settings(&settings)?;
        emit_snapshot(&app);
    }
    Ok(())
}

/// 删除账号：先改设置落盘，再清该账号全部本地数据（keyring 槽位 / OAuth 文件 /
/// cache / history），内存态同步收敛；最后广播最新面板状态
#[tauri::command]
pub fn delete_account(app: AppHandle, account_id: String) -> Result<(), String> {
    let mut settings = storage::load_settings().unwrap_or_default();
    let Some(removed) = settings.remove_account(&account_id) else {
        return Err("账号不存在".to_string());
    };
    storage::save_settings(&settings)?;
    tracing::info!("账号已删除: {}（{}）", removed.name, removed.id);

    // 该账号若有进行中的设备码登录流程，一并取消
    {
        let state = app.state::<AppState>();
        let mut dl = state.device_login.lock().unwrap();
        if dl.account_id.as_deref() == Some(account_id.as_str()) {
            if let Some(tx) = dl.cancel.take() {
                let _ = tx.send(true);
            }
            dl.state = DeviceLoginState::idle();
            dl.account_id = None;
        }
    }

    kimicodebar::accounts::purge_account_data(&account_id);
    // 进程内的网页 access_token 缓存同样按账号收敛，不留残留
    WEB_ACCESS_CACHE.lock().unwrap().remove(&account_id);
    app.state::<AppState>().remove_account_runtime(&account_id);
    emit_snapshot(&app);
    Ok(())
}

/// 切换该账号的登录方式（"api_key" / "oauth"；其他值视为未显式选择）
#[tauri::command]
pub fn set_account_login_method(
    app: AppHandle,
    account_id: String,
    method: Option<String>,
) -> Result<(), String> {
    let method = match method.as_deref() {
        Some("api_key") => Some("api_key".to_string()),
        Some("oauth") => Some("oauth".to_string()),
        _ => None,
    };
    let mut settings = storage::load_settings().unwrap_or_default();
    let Some(account) = settings.accounts.iter_mut().find(|a| a.id == account_id) else {
        return Err("账号不存在".to_string());
    };
    account.login_method = method;
    storage::save_settings(&settings)?;
    emit_snapshot(&app);
    Ok(())
}

// ---------------------------------------------------------------------------
// 凭证命令（全部按账号 id 定位槽位/文件）
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn set_api_key(account_id: String, key: String) -> Result<(), String> {
    let settings = storage::load_settings().unwrap_or_default();
    let Some(account) = settings.account(&account_id) else {
        return Err("账号不存在".to_string());
    };
    // 按提供商校验：DeepSeek 只查 sk- 前缀，GLM 无固定前缀只查非空，Kimi 保持 sk-kimi-（GOAL 拍板）
    let key = if account.is_deepseek() {
        validate_deepseek_api_key(&key)?
    } else if account.is_glm() {
        validate_glm_api_key(&key)?
    } else {
        validate_api_key(&key)?
    };
    creds::save_api_key(&account_id, key).map_err(|e| e.to_string())?;
    // 只记"已配置"，严禁记录 Key 本身
    tracing::info!("API Key 已配置（账号 {account_id}）");
    Ok(())
}

#[tauri::command]
pub fn clear_api_key(app: AppHandle, account_id: String) -> Result<(), String> {
    creds::clear_api_key(&account_id).map_err(|e| e.to_string())?;
    tracing::info!("API Key 已清除（账号 {account_id}）");
    emit_snapshot(&app);
    Ok(())
}

/// 该账号的凭证配置状态（脱敏 Key + 各方式是否已配置）
#[tauri::command]
pub fn get_credential_status(account_id: String) -> CredentialStatus {
    let settings = storage::load_settings().unwrap_or_default();
    let api_key = creds::load_api_key(&account_id).ok().flatten();
    CredentialStatus {
        login_method: settings
            .account(&account_id)
            .and_then(|a| a.login_method.clone()),
        api_key_configured: api_key.is_some(),
        api_key_masked: api_key.as_deref().map(mask_api_key),
        oauth_configured: matches!(oauth::load_credentials(&account_id), Ok(Some(_))),
        // 新旧体系任一配置都算"已配置"（web_token=kimi-auth / web_refresh_token）
        web_token_configured: matches!(creds::load_web_token(&account_id), Ok(Some(_)))
            || matches!(creds::load_web_refresh_token(&account_id), Ok(Some(_))),
    }
}

/// 保存该账号的网页 token（月度总量用）：接受新体系 refresh_token 或旧体系 kimi-auth。
/// 先规范化，再真实调接口校验；通过后存凭据管理器并触发一次刷新，让月度数据立刻上面板。
///
/// 校验策略（新旧兼容）：
/// - 输入形如 refresh_token → 先调 RefreshToken 续期（拿 access_token 验证有效性），
///   成功后落盘 refresh_token 并用 access_token 拉一次月度；
/// - 输入是旧体系 kimi-auth（JWT 未过期仍可作 Bearer 用）→ 直接调 GetSubscriptionStats 校验。
#[tauri::command]
pub async fn set_web_token(
    app: AppHandle,
    account_id: String,
    token: String,
) -> Result<MonthlyInfo, String> {
    let token = web::normalize_web_token(&token)?;

    // 优先按新体系处理：refresh_token 续期成功 → 存 refresh_token + 拉月度
    match web::refresh_access_token(&token).await {
        Ok(session) => {
            creds::save_web_refresh_token(&account_id, &session.refresh_token)
                .map_err(|e| e.to_string())?;
            // 预热该账号的 access_token 缓存，避免随后的 do_refresh 再续一次
            let exp = web::jwt_exp_secs(&session.access_token)
                .unwrap_or(now_unix() + WEB_ACCESS_TOKEN_TTL_SECS);
            WEB_ACCESS_CACHE
                .lock()
                .unwrap()
                .insert(account_id.clone(), (session.access_token.clone(), exp));
            // 只记"已配置"，严禁记录 token 本身
            tracing::info!("网页 refresh_token 已配置（账号 {account_id}）");
            let info = web::fetch_subscription_stats(&session.access_token)
                .await
                .map_err(|e| web_error_message(&e))?;
            do_refresh(&app).await;
            Ok(info)
        }
        // 续期 401/403：可能是旧体系 kimi-auth（JWT 未过期仍可作 Bearer 用），回退直接校验
        Err(WebError::Unauthorized) => match web::fetch_subscription_stats(&token).await {
            Ok(info) => {
                creds::save_web_token(&account_id, &token).map_err(|e| e.to_string())?;
                tracing::info!("网页 kimi-auth token 已配置（账号 {account_id}）");
                do_refresh(&app).await;
                Ok(info)
            }
            Err(WebError::Unauthorized) => {
                Err("网页登录态无效或已过期，请复制最新的 refresh_token 值".to_string())
            }
            Err(e) => Err(web_error_message(&e)),
        },
        // 续期网络/响应异常：此刻无法验证 token 有效性，直接报错（不回退误判 kimi-auth）
        Err(WebError::Http(_)) => Err("网络错误，校验失败".to_string()),
        Err(WebError::Refresh(msg)) => Err(format!("续期失败：{msg}")),
        // Parse 理论不可达（refresh_access_token 不返回 Parse），按续期失败文案兜底
        Err(WebError::Parse(_)) => Err("续期响应解析失败，请重新复制 refresh_token".to_string()),
    }
}

/// 清除该账号的网页 token（新旧体系都清）并触发一次刷新（该页回到无月度数据态）
#[tauri::command]
pub async fn clear_web_token(app: AppHandle, account_id: String) -> Result<(), String> {
    creds::clear_web_token(&account_id).map_err(|e| e.to_string())?;
    creds::clear_web_refresh_token(&account_id).map_err(|e| e.to_string())?;
    WEB_ACCESS_CACHE.lock().unwrap().remove(&account_id);
    tracing::info!("网页 token 已清除（账号 {account_id}）");
    do_refresh(&app).await;
    Ok(())
}

/// 把 WebError 翻译为面向用户的中文文案（与 set_web_token 的返回语义一致）
fn web_error_message(e: &WebError) -> String {
    match e {
        WebError::Unauthorized => {
            "网页登录态无效或已过期，请重新复制 refresh_token 的值".to_string()
        }
        WebError::Http(_) => "网络错误，校验失败".to_string(),
        WebError::Parse(msg) | WebError::Refresh(msg) => msg.clone(),
    }
}

/// 发起设备码登录（绑定指定账号）：先取设备授权码，立即返回 waiting 状态；
/// 后台任务轮询 token，结果经 device-login-updated 事件推送。
/// 交互式弹窗流程，同一时间只跑一个（新发起的会顶替旧流程）
#[tauri::command]
pub async fn start_device_login(app: AppHandle, account_id: String) -> DeviceLoginState {
    if storage::load_settings()
        .unwrap_or_default()
        .account(&account_id)
        .is_none()
    {
        return DeviceLoginState::error("账号不存在".to_string());
    }
    let info = match oauth::start_device_auth().await {
        Ok(info) => info,
        Err(e) => {
            tracing::warn!("设备码登录发起失败: {e}");
            return DeviceLoginState::error(e.to_string());
        }
    };
    tracing::info!("设备码登录已发起（账号 {account_id}），等待用户授权");

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
        dl.account_id = Some(account_id.clone());
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
        dl.account_id = None;
    }
    let _ = app.emit("device-login-updated", DeviceLoginState::idle());
}

#[tauri::command]
pub fn oauth_logout(app: AppHandle, account_id: String) {
    if let Err(e) = oauth::clear_credentials(&account_id) {
        tracing::warn!("清除 OAuth 凭证失败: {e}");
    } else {
        tracing::info!("OAuth 凭证已清除（账号 {account_id}）");
    }
    // 该账号以 oauth 登录：回退为未显式选择（自动优先 api_key）
    let mut settings = storage::load_settings().unwrap_or_default();
    if let Some(account) = settings.accounts.iter_mut().find(|a| a.id == account_id) {
        if account.login_method.as_deref() == Some("oauth") {
            account.login_method = None;
            if let Err(e) = storage::save_settings(&settings) {
                tracing::warn!("保存登录方式失败: {e}");
            }
        }
    }
    // 让该页回到无凭证态（手动组状态发事件）
    emit_snapshot(&app);
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
    let path = crate::diagnostics::export(&state.snapshot(), state.raw_responses())?;
    // 文件已写好；定位失败只记日志，不影响返回路径
    if let Err(e) = app.opener().reveal_item_in_dir(&path) {
        tracing::warn!("定位诊断文件失败: {e}");
    }
    tracing::info!("诊断报告已导出: {}", path.display());
    Ok(path.to_string_lossy().to_string())
}

/// 导出用量报告：各账号 history-<id>.json 采样点写为 CSV（附原文）到
/// {config_dir}/exports/，用系统文件管理器定位目录，返回目录路径
#[tauri::command]
pub fn export_usage_report(app: AppHandle) -> Result<String, String> {
    use tauri_plugin_opener::OpenerExt;
    let dir = kimicodebar::local_usage::export_usage_report()?;
    // 目录已写好；定位失败只记日志，不影响返回路径
    if let Err(e) = app.opener().reveal_item_in_dir(&dir) {
        tracing::warn!("定位导出目录失败: {e}");
    }
    tracing::info!("用量报告已导出: {}", dir.display());
    Ok(dir.to_string_lossy().to_string())
}

// ---------------------------------------------------------------------------
// 以下为内部实现
// ---------------------------------------------------------------------------

/// 把当前内存态广播给前端（账号/凭证变更后调用，面板页即时收敛）
fn emit_snapshot(app: &AppHandle) {
    let panel = app.state::<AppState>().snapshot();
    let _ = app.emit("quota-updated", &panel);
}

/// Kimi Code API Key 前缀（与开放平台 sk- 不通用）
const API_KEY_PREFIX: &str = "sk-kimi-";
/// Key 校验失败时返回给前端的文案
const INVALID_API_KEY_MESSAGE: &str =
    "无效 Key：Kimi Code API Key 以 sk-kimi- 开头（与开放平台 sk- 不通用），请到 kimi.com/code/console 获取";
/// DeepSeek 开放平台 API Key 前缀
const DEEPSEEK_API_KEY_PREFIX: &str = "sk-";
/// DeepSeek Key 校验失败时返回给前端的文案
const INVALID_DEEPSEEK_API_KEY_MESSAGE: &str =
    "无效 Key：DeepSeek API Key 以 sk- 开头，请到 platform.deepseek.com/api_keys 获取";
/// GLM Key 校验失败时返回给前端的文案（GLM Key 无固定前缀，只拦空白输入）
const INVALID_GLM_API_KEY_MESSAGE: &str =
    "无效 Key：GLM API Key 不能为空，请到 bigmodel.cn 用户中心获取";

/// trim 后校验前缀，返回可用的 Key 切片
fn validate_api_key(key: &str) -> Result<&str, String> {
    let key = key.trim();
    if key.starts_with(API_KEY_PREFIX) {
        Ok(key)
    } else {
        Err(INVALID_API_KEY_MESSAGE.to_string())
    }
}

/// DeepSeek Key：trim 后只查 sk- 前缀（GOAL 拍板），返回可用的 Key 切片
fn validate_deepseek_api_key(key: &str) -> Result<&str, String> {
    let key = key.trim();
    if key.starts_with(DEEPSEEK_API_KEY_PREFIX) {
        Ok(key)
    } else {
        Err(INVALID_DEEPSEEK_API_KEY_MESSAGE.to_string())
    }
}

/// GLM Key：无固定前缀（线上为 id.secret 点分两段），trim 后非空即可（GOAL 拍板），
/// 返回可用的 Key 切片
fn validate_glm_api_key(key: &str) -> Result<&str, String> {
    let key = key.trim();
    if key.is_empty() {
        Err(INVALID_GLM_API_KEY_MESSAGE.to_string())
    } else {
        Ok(key)
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
                // 取本流程绑定的账号：丢失（被顶替）时不落盘
                let account_id = {
                    let state = app.state::<AppState>();
                    let dl = state.device_login.lock().unwrap();
                    dl.account_id.clone()
                };
                match account_id {
                    Some(account_id) => match oauth::save_credentials(&account_id, &credentials) {
                        Ok(()) => {
                            // 该账号登录方式切到 oauth 并落盘，数据链路随即生效
                            let mut settings = storage::load_settings().unwrap_or_default();
                            if let Some(account) =
                                settings.accounts.iter_mut().find(|a| a.id == account_id)
                            {
                                account.login_method = Some("oauth".to_string());
                                if let Err(e) = storage::save_settings(&settings) {
                                    tracing::warn!("保存登录方式失败: {e}");
                                }
                            }
                            DeviceLoginState::success()
                        }
                        Err(e) => DeviceLoginState::error(format!("保存凭证失败: {e}")),
                    },
                    None => DeviceLoginState::idle(),
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
            if outcome.status != "waiting" {
                dl.account_id = None;
            }
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

/// 单次单账号配额刷新的三种结局
enum FetchOutcome {
    /// 未配置任何凭证
    NoCredential,
    /// 拉取成功（配额, epoch 秒, usages 原始响应）；装箱避免撑大枚举
    Success(Box<(KimiQuota, i64, String)>),
    /// DeepSeek 余额拉取成功（余额, epoch 秒）
    DeepSeekSuccess(Box<(DeepSeekBalance, i64)>),
    /// 拉取失败（面向用户的中文错误）
    Failed(String),
}

/// 单账号月度刷新的四种结局（独立于配额刷新结果处理）
enum MonthlyOutcome {
    /// 未配置网页 token（新旧体系均无）
    NoToken,
    /// 拉取成功
    Success(MonthlyInfo),
    /// 网页登录态失效（401/403）：保留旧数据，提示更新
    Unauthorized,
    /// 其他失败（网络/解析/keyring）：保留旧数据
    Failed,
}

/// 网页 access_token 提前续期余量（秒）：剩余有效期小于该值视为临期
const WEB_REFRESH_MARGIN_SECS: i64 = 300;
/// access_token 兜底有效期（秒）：JWT 解不出 exp 时按新体系 15 分钟估算
const WEB_ACCESS_TOKEN_TTL_SECS: i64 = 15 * 60;
/// 进程内网页 access_token 缓存：账号 id → (access_token, expires_at 秒)。
/// BTreeMap::new 是 const，可直接静态初始化（无新依赖）。
/// refresh_token 轮换后的新值由调用方负责落盘（见 fetch_monthly_with_refresh）。
static WEB_ACCESS_CACHE: Mutex<BTreeMap<String, (String, i64)>> = Mutex::new(BTreeMap::new());

/// 缓存命中（纯函数便于单测）：存在且剩余有效期大于续期余量才可用
fn web_cache_fresh<'a>(
    cache: &'a BTreeMap<String, (String, i64)>,
    account_id: &str,
    now: i64,
) -> Option<&'a str> {
    cache
        .get(account_id)
        .filter(|(_, exp)| *exp - now > WEB_REFRESH_MARGIN_SECS)
        .map(|(token, _)| token.as_str())
}

/// 缓存兜底（纯函数）：未过期即可用（续期失败时的网络抖动场景）
fn web_cache_stale<'a>(
    cache: &'a BTreeMap<String, (String, i64)>,
    account_id: &str,
    now: i64,
) -> Option<&'a str> {
    cache
        .get(account_id)
        .filter(|(_, exp)| *exp > now)
        .map(|(token, _)| token.as_str())
}

/// 取该账号的网页 token 并拉取月度总量；未配置 token → NoToken。
///
/// 新鉴权体系（refresh_token）优先：进程内按账号缓存 access_token，临期自动调
/// RefreshToken 续期并把轮换后的新 refresh_token 落盘（轮换制丢旧即失效）；
/// 旧体系（kimi-auth，未过期仍可作 Bearer 用）作为兼容路径保留。
async fn fetch_monthly(account_id: &str) -> MonthlyOutcome {
    if let Ok(Some(refresh_token)) = creds::load_web_refresh_token(account_id) {
        return fetch_monthly_with_refresh(account_id, &refresh_token).await;
    }
    let token = match creds::load_web_token(account_id) {
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

/// 新鉴权体系月度刷新：access_token 未临期直接用缓存，否则续期后查询。
async fn fetch_monthly_with_refresh(account_id: &str, refresh_token: &str) -> MonthlyOutcome {
    let now = now_unix();
    let access_token = match web_access_token(account_id, refresh_token, now).await {
        Ok(token) => token,
        Err(MonthlyOutcome::Unauthorized) => {
            // refresh_token 已失效：清掉本地凭证，下次引导重新粘贴
            tracing::warn!("网页 refresh_token 已失效，已清除本地凭证（账号 {account_id}）");
            let _ = creds::clear_web_refresh_token(account_id);
            return MonthlyOutcome::Unauthorized;
        }
        Err(other) => return other,
    };

    match web::fetch_subscription_stats(&access_token).await {
        Ok(info) => MonthlyOutcome::Success(info),
        // access_token 已过期（缓存判定后提前失效等罕见情况）：清该账号缓存，
        // 用当前 refresh_token 再续一次；仍 401 则判 refresh_token 失效
        Err(WebError::Unauthorized) => {
            WEB_ACCESS_CACHE.lock().unwrap().remove(account_id);
            match web_access_token(account_id, refresh_token, now_unix()).await {
                Ok(token) => match web::fetch_subscription_stats(&token).await {
                    Ok(info) => MonthlyOutcome::Success(info),
                    Err(WebError::Unauthorized) => {
                        tracing::warn!(
                            "网页 refresh_token 已失效，已清除本地凭证（账号 {account_id}）"
                        );
                        let _ = creds::clear_web_refresh_token(account_id);
                        MonthlyOutcome::Unauthorized
                    }
                    Err(_) => MonthlyOutcome::Failed,
                },
                Err(MonthlyOutcome::Unauthorized) => {
                    let _ = creds::clear_web_refresh_token(account_id);
                    MonthlyOutcome::Unauthorized
                }
                Err(other) => other,
            }
        }
        Err(_) => MonthlyOutcome::Failed,
    }
}

/// 取该账号可用的网页 access_token：缓存未临期直接复用，否则调 RefreshToken 续期。
/// 续期成功后把轮换的新 refresh_token 落盘（丢旧即失效）。
async fn web_access_token(
    account_id: &str,
    refresh_token: &str,
    now: i64,
) -> Result<String, MonthlyOutcome> {
    let cached = {
        let cache = WEB_ACCESS_CACHE.lock().unwrap();
        web_cache_fresh(&cache, account_id, now).map(str::to_string)
    };
    if let Some(token) = cached {
        return Ok(token);
    }

    match web::refresh_access_token(refresh_token).await {
        Ok(session) => {
            // 轮换后的新 refresh_token 必须落盘，否则旧值失效后无法再续期
            if let Err(e) = creds::save_web_refresh_token(account_id, &session.refresh_token) {
                tracing::warn!("保存轮换后的 refresh_token 失败: {e}");
            }
            let exp =
                web::jwt_exp_secs(&session.access_token).unwrap_or(now + WEB_ACCESS_TOKEN_TTL_SECS);
            WEB_ACCESS_CACHE
                .lock()
                .unwrap()
                .insert(account_id.to_string(), (session.access_token.clone(), exp));
            // 续期成功留痕（严禁记录 token 本身），便于诊断"自动续期是否在跑"
            tracing::info!("网页 access_token 续期成功（账号 {account_id}）");
            Ok(session.access_token)
        }
        Err(WebError::Unauthorized) => {
            WEB_ACCESS_CACHE.lock().unwrap().remove(account_id);
            Err(MonthlyOutcome::Unauthorized)
        }
        // 网络/解析失败：若缓存里还有未过期的 access_token 可先用（网络抖动场景）
        Err(_) => {
            let stale = {
                let cache = WEB_ACCESS_CACHE.lock().unwrap();
                web_cache_stale(&cache, account_id, now).map(str::to_string)
            };
            match stale {
                Some(token) => Ok(token),
                _ => Err(MonthlyOutcome::Failed),
            }
        }
    }
}

/// 取该账号 token 并拉取配额；失败分支记 warn（错误类型与文案，严禁记录 token）
async fn fetch_with_credential(account: &Account) -> FetchOutcome {
    let token = match creds::get_active_token(account).await {
        Ok(Some((_, token))) => token,
        Ok(None) => return FetchOutcome::NoCredential,
        Err(e) => {
            tracing::warn!("配额刷新失败（账号 {}）: 读取凭证失败: {e}", account.id);
            return FetchOutcome::Failed(e.to_string());
        }
    };
    // DeepSeek 账号走余额接口（Kimi 逻辑零改动，下方原样）
    if account.is_deepseek() {
        return match DeepSeekClient::new().fetch_balance(&token).await {
            Ok(balance) => FetchOutcome::DeepSeekSuccess(Box::new((balance, now_unix()))),
            Err(QuotaError::Unauthorized) => {
                tracing::warn!(
                    "余额刷新失败（账号 {}）: 凭证无效或已过期 (Unauthorized)",
                    account.id
                );
                FetchOutcome::Failed("凭证无效或已过期，请在设置中重新配置".to_string())
            }
            Err(QuotaError::Http(e)) => {
                tracing::warn!("余额刷新失败（账号 {}）: 网络错误: {e}", account.id);
                FetchOutcome::Failed("网络错误，展示缓存数据".to_string())
            }
            // Parse / Api（含 429 限流）：展示原始错误文本
            Err(other) => {
                tracing::warn!("余额刷新失败（账号 {}）: {other}", account.id);
                FetchOutcome::Failed(other.to_string())
            }
        };
    }
    // GLM 账号走套餐额度接口，产出映射进 KimiQuota 契约（five_hour/weekly/membership_level），
    // 成功结局复用 FetchOutcome::Success：下游与 Kimi 配额全链同构
    // （写缓存、写历史采样、低额判定、5h 重置提醒），只跳过月度拉取（无此接口）
    if account.is_glm() {
        return match GlmClient::new().fetch_quota_with_raw(&token).await {
            Ok((quota, raw)) => FetchOutcome::Success(Box::new((quota, now_unix(), raw))),
            Err(QuotaError::Unauthorized) => {
                tracing::warn!(
                    "GLM 额度刷新失败（账号 {}）: 凭证无效或已过期 (Unauthorized)",
                    account.id
                );
                FetchOutcome::Failed("凭证无效或已过期，请在设置中重新配置".to_string())
            }
            Err(QuotaError::Http(e)) => {
                tracing::warn!("GLM 额度刷新失败（账号 {}）: 网络错误: {e}", account.id);
                FetchOutcome::Failed("网络错误，展示缓存数据".to_string())
            }
            // Parse / Api（含 429 限流）：展示原始错误文本
            Err(other) => {
                tracing::warn!("GLM 额度刷新失败（账号 {}）: {other}", account.id);
                FetchOutcome::Failed(other.to_string())
            }
        };
    }
    match KimiClient::new().fetch_quota_with_raw(&token).await {
        Ok((quota, raw)) => FetchOutcome::Success(Box::new((quota, now_unix(), raw))),
        Err(QuotaError::Unauthorized) => {
            tracing::warn!(
                "配额刷新失败（账号 {}）: 凭证无效或已过期 (Unauthorized)",
                account.id
            );
            FetchOutcome::Failed("凭证无效或已过期，请在设置中重新配置".to_string())
        }
        Err(QuotaError::Http(e)) => {
            tracing::warn!("配额刷新失败（账号 {}）: 网络错误: {e}", account.id);
            FetchOutcome::Failed("网络错误，展示缓存数据".to_string())
        }
        // Parse / Api：展示原始错误文本
        Err(other) => {
            tracing::warn!("配额刷新失败（账号 {}）: {other}", account.id);
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
    assemble_panel_state_with(inner, &settings, &has_any_credential)
}

/// 组装实现（设置与凭证查询入参化，纯函数便于单测）：
/// 按 settings.accounts 顺序逐账号取运行时快照；低额判定要求该账号最近一次
/// 刷新成功（error 为空）——拉取失败/凭证无效的账号不算低额（GOAL 拍板）。
/// DeepSeek 账号改按余额判定（不可用或总余额低于阈值元）；GLM 账号走 Kimi 同款
/// 配额判定（映射后的 KimiQuota 剩余百分比 < warn_threshold_pct），Kimi 判定逻辑不变。
fn assemble_panel_state_with(
    inner: &Inner,
    settings: &storage::Settings,
    has_credential: &dyn Fn(&str) -> bool,
) -> PanelState {
    let threshold = settings.warn_threshold_pct;
    let deepseek_threshold = settings.deepseek_warn_threshold;
    let accounts = settings
        .accounts
        .iter()
        .map(|account| {
            let runtime = inner.accounts.get(&account.id);
            let (quota, fetched_at) = match runtime.and_then(|rt| rt.last_quota.as_ref()) {
                Some((quota, fetched_at)) => (Some(quota.clone()), Some(*fetched_at)),
                // 无配额时取余额时间（DeepSeek 账号的"上次成功刷新"）
                None => (
                    None,
                    runtime.and_then(|rt| rt.last_balance.as_ref().map(|(_, t)| *t)),
                ),
            };
            let error = runtime.and_then(|rt| rt.error.clone());
            let monthly = runtime.and_then(|rt| rt.monthly.clone());
            let monthly_error = runtime.and_then(|rt| rt.monthly_error.clone());
            let deepseek_balance = runtime
                .and_then(|rt| rt.last_balance.clone())
                .map(|(b, _)| b);
            let low_warning = if account.is_deepseek() {
                error.is_none()
                    && deepseek_balance
                        .as_ref()
                        .is_some_and(|b| deepseek_needs_low_warning(b, deepseek_threshold))
            } else {
                error.is_none()
                    && (quota
                        .as_ref()
                        .is_some_and(|q| needs_low_warning(q, threshold))
                        || monthly
                            .as_ref()
                            .is_some_and(|m| m.total_pct >= 100.0 - threshold))
            };
            AccountPanel {
                account: account.clone(),
                credential: has_credential(&account.id),
                quota,
                fetched_at,
                error,
                low_warning,
                monthly,
                monthly_error,
                deepseek_balance,
            }
        })
        .collect();
    PanelState {
        loading: inner.loading,
        accounts,
    }
}

/// 托盘变红判定：任一账号低额（低额本身已排除刷新失败的账号）
pub(crate) fn any_low_warning(panel: &PanelState) -> bool {
    panel.accounts.iter().any(|a| a.low_warning)
}

/// 该账号配额中最差（最低）的剩余百分比：weekly / five_hour / total 三者最小
fn worst_window_pct(quota: &KimiQuota) -> Option<f64> {
    [
        quota.weekly.as_ref().map(|d| d.percent_remaining),
        quota.five_hour.as_ref().map(|d| d.percent_remaining),
        quota.total.as_ref().map(|t| t.percent_remaining),
    ]
    .into_iter()
    .flatten()
    .reduce(f64::min)
}

/// 托盘 tooltip 的附加行：最差账号（剩余百分比最低、且最近刷新成功）的摘要，
/// 如 "\n7天剩余 87% · 5h剩余 36%"；多账号时前缀账号名（"\n账号 1 · 7天剩余 8%"）。
/// DeepSeek 账号以总余额参与"最差"比较（余额不可用记 0，与百分比同向即越小越差，
/// 仅是挑展示对象的启发式），摘要如 "\nDeepSeek 余额 ¥3.20"。
/// 全部账号无可用数据时为 None
pub(crate) fn worst_account_tooltip(panel: &PanelState, lang: i18n::Lang) -> Option<String> {
    let (account, summary) = panel
        .accounts
        .iter()
        .filter(|a| a.error.is_none())
        .filter_map(|a| {
            if a.account.is_deepseek() {
                let balance = a.deepseek_balance.as_ref()?;
                let severity = if balance.is_available {
                    balance.total_balance
                } else {
                    0.0
                };
                Some((a, severity, i18n::deepseek_summary(lang, balance)))
            } else {
                let quota = a.quota.as_ref()?;
                worst_window_pct(quota).map(|pct| (a, pct, i18n::quota_summary(lang, quota)))
            }
        })
        .min_by(|x, y| x.1.partial_cmp(&y.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(a, _, summary)| (a, summary))?;
    if summary.is_empty() {
        return None;
    }
    if panel.accounts.len() > 1 {
        Some(format!("\n{} · {}", account.account.name, summary))
    } else {
        Some(format!("\n{summary}"))
    }
}

/// 该账号是否已配置任一凭证（API Key 或 OAuth 本地凭证）
fn has_any_credential(account_id: &str) -> bool {
    if matches!(creds::load_api_key(account_id), Ok(Some(_))) {
        return true;
    }
    matches!(oauth::load_credentials(account_id), Ok(Some(_)))
}

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kimicodebar::quota::{QuotaDetail, TotalQuotaInfo};

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

    // ---- AppSettings DTO 双向转换 ----

    #[test]
    fn app_settings_dto_roundtrip_covers_minimal_mode() {
        // storage → DTO：极简模式字段透出
        let stored = storage::Settings {
            minimal_mode: true,
            ..Default::default()
        };
        let dto = AppSettings::from(stored);
        assert!(dto.minimal_mode);

        // DTO → storage：字段带回（账号列表按约定由调用方补，不在此断言）
        let back = storage::Settings::from(dto);
        assert!(back.minimal_mode);

        // 默认（关）双向一致
        let dto = AppSettings::from(storage::Settings::default());
        assert!(!dto.minimal_mode);
        assert!(!storage::Settings::from(dto).minimal_mode);
    }

    // ---- 多账号状态组装 ----

    fn account(id: &str, name: &str) -> Account {
        Account {
            id: id.to_string(),
            name: name.to_string(),
            login_method: None,
            provider: "kimi".to_string(),
        }
    }

    fn deepseek_account(id: &str, name: &str) -> Account {
        Account {
            provider: "deepseek".to_string(),
            ..account(id, name)
        }
    }

    fn deepseek_balance(is_available: bool, total: f64) -> DeepSeekBalance {
        DeepSeekBalance {
            is_available,
            currency: "CNY".to_string(),
            total_balance: total,
            granted_balance: 0.0,
            topped_up_balance: total,
        }
    }

    fn quota_with_weekly(percent_remaining: f64) -> KimiQuota {
        KimiQuota {
            weekly: Some(QuotaDetail {
                used: 100.0 - percent_remaining,
                limit: 100.0,
                remaining: percent_remaining,
                reset_time: None,
                percent_remaining,
            }),
            ..Default::default()
        }
    }

    fn settings_with_accounts(accounts: Vec<Account>, threshold: f64) -> storage::Settings {
        storage::Settings {
            accounts,
            warn_threshold_pct: threshold,
            ..Default::default()
        }
    }

    #[test]
    fn assemble_multi_account_panel_state() {
        let a1 = account("id-1", "账号 1");
        let a2 = account("id-2", "工作号");
        let settings = settings_with_accounts(vec![a1.clone(), a2.clone()], 20.0);

        let mut inner = Inner {
            loading: true,
            ..Inner::default()
        };
        // 账号 1：健康数据（剩余 87%），月度有值
        inner.accounts.insert(
            "id-1".to_string(),
            AccountRuntime {
                error: None,
                last_quota: Some((quota_with_weekly(87.0), 1_900_000_000)),
                last_raw_response: Some(("raw".to_string(), 1_900_000_000)),
                monthly: Some(MonthlyInfo {
                    total_pct: 16.12,
                    kimi_pct: 11.12,
                    code_pct: 5.0,
                    reset_time: None,
                }),
                monthly_error: None,
                last_balance: None,
            },
        );
        // 账号 2：拉取失败（无效 token），残留旧缓存数据
        inner.accounts.insert(
            "id-2".to_string(),
            AccountRuntime {
                error: Some("凭证无效或已过期，请在设置中重新配置".to_string()),
                last_quota: Some((quota_with_weekly(5.0), 1_899_000_000)),
                last_raw_response: None,
                monthly: None,
                monthly_error: Some("月度数据刷新失败".to_string()),
                last_balance: None,
            },
        );

        let panel = assemble_panel_state_with(&inner, &settings, &|id| id == "id-1");

        assert!(panel.loading);
        assert_eq!(panel.accounts.len(), 2);

        let p1 = &panel.accounts[0];
        assert_eq!(p1.account.id, "id-1");
        assert_eq!(p1.account.name, "账号 1");
        assert!(p1.credential);
        assert!(p1.error.is_none());
        assert_eq!(p1.fetched_at, Some(1_900_000_000));
        assert!(!p1.low_warning, "剩余 87% 不应低额");
        assert!((p1.monthly.as_ref().unwrap().total_pct - 16.12).abs() < 1e-9);
        assert!(p1.monthly_error.is_none());

        let p2 = &panel.accounts[1];
        assert_eq!(p2.account.id, "id-2");
        assert!(!p2.credential);
        assert_eq!(
            p2.error.as_deref(),
            Some("凭证无效或已过期，请在设置中重新配置")
        );
        // 缓存数据照常展示
        assert!(p2.quota.is_some());
        assert!(!p2.low_warning, "拉取失败的账号不算低额（GOAL 拍板）");
        assert_eq!(p2.monthly_error.as_deref(), Some("月度数据刷新失败"));
    }

    #[test]
    fn low_warning_any_account_triggers_alert_but_failed_account_never_low() {
        let a1 = account("id-1", "账号 1");
        let a2 = account("id-2", "账号 2");
        let settings = settings_with_accounts(vec![a1, a2], 20.0);

        let mut inner = Inner::default();
        // 账号 1：刷新成功但剩余 8%（低额）→ 该账号 low
        inner.accounts.insert(
            "id-1".to_string(),
            AccountRuntime {
                error: None,
                last_quota: Some((quota_with_weekly(8.0), 1_900_000_000)),
                ..AccountRuntime::default()
            },
        );
        // 账号 2：刷新失败 + 缓存剩余 3% → 不算低额
        inner.accounts.insert(
            "id-2".to_string(),
            AccountRuntime {
                error: Some("网络错误，展示缓存数据".to_string()),
                last_quota: Some((quota_with_weekly(3.0), 1_899_000_000)),
                ..AccountRuntime::default()
            },
        );

        let panel = assemble_panel_state_with(&inner, &settings, &|_| true);
        assert!(panel.accounts[0].low_warning);
        assert!(!panel.accounts[1].low_warning);
        assert!(any_low_warning(&panel), "任一账号低额 → 托盘变红");

        // 反向验证场景：唯一"低额"的账号是拉取失败的账号 → 托盘不变红
        let settings2 = settings_with_accounts(vec![account("id-2", "账号 2")], 20.0);
        let panel2 = assemble_panel_state_with(&inner, &settings2, &|_| true);
        assert!(!panel2.accounts[0].low_warning);
        assert!(!any_low_warning(&panel2), "失败账号的低额不触发变红");
    }

    #[test]
    fn low_warning_monthly_counts_when_fresh() {
        let settings = settings_with_accounts(vec![account("id-1", "账号 1")], 20.0);
        let mut inner = Inner::default();
        inner.accounts.insert(
            "id-1".to_string(),
            AccountRuntime {
                error: None,
                monthly: Some(MonthlyInfo {
                    total_pct: 85.0, // 已用 85% ≥ 100-20 → 低额
                    kimi_pct: 80.0,
                    code_pct: 5.0,
                    reset_time: None,
                }),
                ..AccountRuntime::default()
            },
        );
        let panel = assemble_panel_state_with(&inner, &settings, &|_| true);
        assert!(panel.accounts[0].low_warning);
    }

    #[test]
    fn tooltip_picks_worst_account_and_prefixes_name_only_when_multi() {
        let a1 = account("id-1", "账号 1");
        let a2 = account("id-2", "工作号");
        let settings = settings_with_accounts(vec![a1, a2], 20.0);

        let mut inner = Inner::default();
        inner.accounts.insert(
            "id-1".to_string(),
            AccountRuntime {
                error: None,
                last_quota: Some((quota_with_weekly(50.0), 1)),
                ..AccountRuntime::default()
            },
        );
        inner.accounts.insert(
            "id-2".to_string(),
            AccountRuntime {
                error: None,
                last_quota: Some((quota_with_weekly(9.0), 1)),
                ..AccountRuntime::default()
            },
        );

        let panel = assemble_panel_state_with(&inner, &settings, &|_| true);
        let tip = worst_account_tooltip(&panel, i18n::Lang::Zh).unwrap();
        // 最差账号是"工作号"（9%），多账号时前缀账号名
        assert!(tip.starts_with("\n工作号 · "), "实际: {tip}");
        assert!(tip.contains("9%"), "实际: {tip}");

        // 单账号无前缀
        let settings1 = settings_with_accounts(vec![account("id-1", "账号 1")], 20.0);
        let panel1 = assemble_panel_state_with(&inner, &settings1, &|_| true);
        let tip1 = worst_account_tooltip(&panel1, i18n::Lang::Zh).unwrap();
        assert!(tip1.starts_with("\n7天剩余"), "实际: {tip1}");

        // 全部账号刷新失败：无 tooltip 摘要
        let mut inner_fail = Inner::default();
        inner_fail.accounts.insert(
            "id-1".to_string(),
            AccountRuntime {
                error: Some("网络错误，展示缓存数据".to_string()),
                last_quota: Some((quota_with_weekly(9.0), 1)),
                ..AccountRuntime::default()
            },
        );
        let panel_fail = assemble_panel_state_with(&inner_fail, &settings1, &|_| true);
        assert!(worst_account_tooltip(&panel_fail, i18n::Lang::Zh).is_none());
    }

    #[test]
    fn assemble_without_accounts_is_empty_page_list() {
        let inner = Inner::default();
        let settings = storage::Settings::default();
        let panel = assemble_panel_state_with(&inner, &settings, &|_| false);
        assert!(panel.accounts.is_empty());
        assert!(!panel.loading);
        assert!(!any_low_warning(&panel));
        assert!(worst_account_tooltip(&panel, i18n::Lang::En).is_none());
    }

    // ---- 月度 access_token 缓存按账号隔离 ----

    #[test]
    fn web_access_cache_isolated_per_account() {
        let mut cache: BTreeMap<String, (String, i64)> = BTreeMap::new();
        let now = 1_000_000;
        let valid_a = now + 3600;
        let soon_b = now + 100; // 余 100s < 300s 余量：fresh 不命中、stale 命中

        cache.insert("acc-a".to_string(), ("token-a".to_string(), valid_a));
        cache.insert("acc-b".to_string(), ("token-b".to_string(), soon_b));

        assert_eq!(web_cache_fresh(&cache, "acc-a", now), Some("token-a"));
        assert_eq!(web_cache_fresh(&cache, "acc-b", now), None, "临期不应命中");
        assert_eq!(web_cache_stale(&cache, "acc-b", now), Some("token-b"));
        assert_eq!(web_cache_fresh(&cache, "acc-c", now), None);

        // 清 acc-a 不影响 acc-b
        cache.remove("acc-a");
        assert_eq!(web_cache_fresh(&cache, "acc-a", now), None);
        assert_eq!(web_cache_stale(&cache, "acc-b", now), Some("token-b"));

        // 已过期（exp ≤ now）：stale 也不命中
        cache.insert("acc-d".to_string(), ("token-d".to_string(), now - 1));
        assert_eq!(web_cache_stale(&cache, "acc-d", now), None);
    }

    // ---- 删账号后内存态收敛 ----

    #[test]
    fn remove_account_runtime_leaves_no_residue() {
        let state = AppState {
            inner: Mutex::new(Inner::default()),
            device_login: Mutex::new(DeviceLoginInner::default()),
            refresh_lock: tokio::sync::Mutex::new(()),
        };
        {
            let mut inner = state.inner.lock().unwrap();
            inner
                .accounts
                .insert("id-1".to_string(), AccountRuntime::default());
            inner
                .accounts
                .insert("id-2".to_string(), AccountRuntime::default());
        }

        state.remove_account_runtime("id-1");
        let inner = state.inner.lock().unwrap();
        assert!(!inner.accounts.contains_key("id-1"));
        assert!(inner.accounts.contains_key("id-2"));
        // 幂等：再删一次不 panic
        drop(inner);
        state.remove_account_runtime("id-1");
    }

    // ---- total 窗口也参与最差百分比（worst_window_pct） ----

    #[test]
    fn worst_window_pct_covers_total() {
        let q = KimiQuota {
            weekly: Some(QuotaDetail {
                percent_remaining: 50.0,
                ..Default::default()
            }),
            total: Some(TotalQuotaInfo {
                limit: 500.0,
                remaining: 25.0,
                percent_remaining: 5.0,
            }),
            ..Default::default()
        };
        assert_eq!(worst_window_pct(&q), Some(5.0));
        assert_eq!(worst_window_pct(&KimiQuota::default()), None);
    }

    // ---- DeepSeek Key 校验 ----

    #[test]
    fn validate_deepseek_api_key_accepts_sk_prefix() {
        assert_eq!(validate_deepseek_api_key("sk-abc123").unwrap(), "sk-abc123");
        // trim 后校验
        assert_eq!(
            validate_deepseek_api_key("  sk-abc123 \n").unwrap(),
            "sk-abc123"
        );
    }

    #[test]
    fn validate_deepseek_api_key_rejects_non_sk() {
        let err = validate_deepseek_api_key("pk-abcdef").unwrap_err();
        assert_eq!(err, INVALID_DEEPSEEK_API_KEY_MESSAGE);
        assert!(validate_deepseek_api_key("").is_err());
        assert!(validate_deepseek_api_key("   ").is_err());
    }

    // ---- AppSettings DTO：DeepSeek 阈值双向转换 ----

    #[test]
    fn app_settings_dto_roundtrip_covers_deepseek_threshold() {
        let stored = storage::Settings {
            deepseek_warn_threshold: 12.5,
            ..Default::default()
        };
        let dto = AppSettings::from(stored);
        assert!((dto.deepseek_warn_threshold - 12.5).abs() < 1e-9);
        let back = storage::Settings::from(dto);
        assert!((back.deepseek_warn_threshold - 12.5).abs() < 1e-9);

        // 默认 5 元双向一致
        let dto = AppSettings::from(storage::Settings::default());
        assert!((dto.deepseek_warn_threshold - 5.0).abs() < 1e-9);
    }

    // ---- DeepSeek 账号的面板组装与低额判定 ----

    #[test]
    fn deepseek_panel_low_below_threshold_and_unavailable() {
        let acc = deepseek_account("ds-1", "DeepSeek 号");
        let mut settings = settings_with_accounts(vec![acc], 20.0);
        settings.deepseek_warn_threshold = 5.0;

        let mut inner = Inner::default();
        // 余额 ¥3.20 < 阈值 ¥5：低额，且余额透传到面板
        inner.accounts.insert(
            "ds-1".to_string(),
            AccountRuntime {
                error: None,
                last_balance: Some((deepseek_balance(true, 3.20), 1_900_000_000)),
                ..AccountRuntime::default()
            },
        );
        let panel = assemble_panel_state_with(&inner, &settings, &|_| true);
        let p = &panel.accounts[0];
        assert!(p.low_warning, "余额低于阈值应低额");
        assert_eq!(p.fetched_at, Some(1_900_000_000), "无配额时取余额时间");
        assert!(p.quota.is_none(), "DeepSeek 账号 Kimi 字段为空");
        let b = p.deepseek_balance.as_ref().expect("余额应透传");
        assert!((b.total_balance - 3.20).abs() < 1e-9);

        // 余额不可用：无论金额都低额
        inner.accounts.insert(
            "ds-1".to_string(),
            AccountRuntime {
                error: None,
                last_balance: Some((deepseek_balance(false, 100.0), 1_900_000_000)),
                ..AccountRuntime::default()
            },
        );
        let panel = assemble_panel_state_with(&inner, &settings, &|_| true);
        assert!(panel.accounts[0].low_warning, "余额不可用应低额");
    }

    #[test]
    fn deepseek_panel_not_low_above_threshold_or_on_error() {
        let acc = deepseek_account("ds-1", "DeepSeek 号");
        let mut settings = settings_with_accounts(vec![acc], 20.0);
        settings.deepseek_warn_threshold = 5.0;

        let mut inner = Inner::default();
        // 余额 ¥12.34 ≥ 阈值：不红
        inner.accounts.insert(
            "ds-1".to_string(),
            AccountRuntime {
                error: None,
                last_balance: Some((deepseek_balance(true, 12.34), 1_900_000_000)),
                ..AccountRuntime::default()
            },
        );
        let panel = assemble_panel_state_with(&inner, &settings, &|_| true);
        assert!(!panel.accounts[0].low_warning);
        assert!(!any_low_warning(&panel));

        // 拉取失败（凭证无效）+ 缓存余额 ¥0：失败账号永不低额（铁律）
        inner.accounts.insert(
            "ds-1".to_string(),
            AccountRuntime {
                error: Some("凭证无效或已过期，请在设置中重新配置".to_string()),
                last_balance: Some((deepseek_balance(true, 0.0), 1_899_000_000)),
                ..AccountRuntime::default()
            },
        );
        let panel = assemble_panel_state_with(&inner, &settings, &|_| true);
        assert!(!panel.accounts[0].low_warning, "失败账号不算低额");
        assert!(!any_low_warning(&panel), "托盘不应变红");
        // 缓存余额照常展示（错误横幅与数据并存）
        assert!(panel.accounts[0].deepseek_balance.is_some());
    }

    #[test]
    fn kimi_account_has_no_deepseek_balance() {
        let settings = settings_with_accounts(vec![account("id-1", "账号 1")], 20.0);
        let mut inner = Inner::default();
        inner.accounts.insert(
            "id-1".to_string(),
            AccountRuntime {
                error: None,
                last_quota: Some((quota_with_weekly(87.0), 1_900_000_000)),
                ..AccountRuntime::default()
            },
        );
        let panel = assemble_panel_state_with(&inner, &settings, &|_| true);
        assert!(panel.accounts[0].deepseek_balance.is_none());
    }

    #[test]
    fn tooltip_covers_deepseek_balance() {
        let kimi = account("id-1", "账号 1");
        let ds = deepseek_account("ds-1", "DS 号");
        let settings = settings_with_accounts(vec![kimi, ds], 20.0);

        let mut inner = Inner::default();
        inner.accounts.insert(
            "id-1".to_string(),
            AccountRuntime {
                error: None,
                last_quota: Some((quota_with_weekly(50.0), 1)),
                ..AccountRuntime::default()
            },
        );
        inner.accounts.insert(
            "ds-1".to_string(),
            AccountRuntime {
                error: None,
                last_balance: Some((deepseek_balance(true, 3.20), 1)),
                ..AccountRuntime::default()
            },
        );

        // 余额 ¥3.20 比 7天剩余 50% 更"差"（启发式同向比较）：tooltip 取 DeepSeek 摘要
        let panel = assemble_panel_state_with(&inner, &settings, &|_| true);
        let tip = worst_account_tooltip(&panel, i18n::Lang::Zh).unwrap();
        assert_eq!(tip, "\nDS 号 · DeepSeek 余额 ¥3.20", "实际: {tip}");

        // 英文摘要
        let tip_en = worst_account_tooltip(&panel, i18n::Lang::En).unwrap();
        assert!(tip_en.contains("DeepSeek balance ¥3.20"), "实际: {tip_en}");

        // 单个 DeepSeek 账号：无账号名前缀
        let settings_ds = settings_with_accounts(vec![deepseek_account("ds-1", "DS 号")], 20.0);
        let panel_ds = assemble_panel_state_with(&inner, &settings_ds, &|_| true);
        let tip_ds = worst_account_tooltip(&panel_ds, i18n::Lang::Zh).unwrap();
        assert_eq!(tip_ds, "\nDeepSeek 余额 ¥3.20", "实际: {tip_ds}");
    }

    // ---- GLM 账号：Key 校验 / 面板组装与低额判定 ----

    fn glm_account(id: &str, name: &str) -> Account {
        Account {
            provider: "glm".to_string(),
            ..account(id, name)
        }
    }

    /// GLM 映射后的配额形态：5 小时窗 + 周窗 + 档位（以 100 为总量合成的百分比口径）
    fn glm_quota(five_hour_remaining: f64, weekly_remaining: f64) -> KimiQuota {
        let detail = |percent_remaining: f64| QuotaDetail {
            used: 100.0 - percent_remaining,
            limit: 100.0,
            remaining: percent_remaining,
            reset_time: None,
            percent_remaining,
        };
        KimiQuota {
            five_hour: Some(detail(five_hour_remaining)),
            weekly: Some(detail(weekly_remaining)),
            membership_level: Some("pro".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn validate_glm_api_key_accepts_any_nonempty() {
        // GLM Key 无固定前缀（id.secret 点分两段）：任意非空白值都接受，不做 sk- 断言
        assert_eq!(
            validate_glm_api_key("abc123.def456").unwrap(),
            "abc123.def456"
        );
        assert_eq!(validate_glm_api_key("sk-whatever").unwrap(), "sk-whatever");
        // trim 后返回可用切片
        assert_eq!(validate_glm_api_key("  abc.def \n").unwrap(), "abc.def");
    }

    #[test]
    fn validate_glm_api_key_rejects_blank() {
        let err = validate_glm_api_key("").unwrap_err();
        assert_eq!(err, INVALID_GLM_API_KEY_MESSAGE);
        assert!(validate_glm_api_key("   \n ").is_err());
    }

    #[test]
    fn glm_panel_low_when_below_threshold() {
        // GLM 走 Kimi 同款配额判定：任一窗口剩余 < warn_threshold_pct 即低额
        let acc = glm_account("glm-1", "GLM 号");
        let settings = settings_with_accounts(vec![acc], 20.0);

        let mut inner = Inner::default();
        inner.accounts.insert(
            "glm-1".to_string(),
            AccountRuntime {
                error: None,
                last_quota: Some((glm_quota(15.0, 60.0), 1_900_000_000)),
                ..AccountRuntime::default()
            },
        );
        let panel = assemble_panel_state_with(&inner, &settings, &|_| true);
        let p = &panel.accounts[0];
        assert!(p.low_warning, "5h 剩余 15% < 阈值 20% 应低额");
        assert_eq!(p.account.provider, "glm");
        assert!(p.quota.is_some(), "GLM 配额映射进 KimiQuota 契约");
        assert!(p.deepseek_balance.is_none(), "GLM 账号无 DeepSeek 字段");
        assert_eq!(p.fetched_at, Some(1_900_000_000));
        assert!(any_low_warning(&panel), "托盘应变红");
    }

    #[test]
    fn glm_panel_not_low_when_healthy_or_on_error() {
        let acc = glm_account("glm-1", "GLM 号");
        let settings = settings_with_accounts(vec![acc], 20.0);

        let mut inner = Inner::default();
        // 双窗口剩余都高于阈值：不红
        inner.accounts.insert(
            "glm-1".to_string(),
            AccountRuntime {
                error: None,
                last_quota: Some((glm_quota(57.5, 39.0), 1_900_000_000)),
                ..AccountRuntime::default()
            },
        );
        let panel = assemble_panel_state_with(&inner, &settings, &|_| true);
        assert!(!panel.accounts[0].low_warning);

        // 拉取失败 + 缓存配额剩余 0：失败账号永不低额（拉取失败不红铁律）
        inner.accounts.insert(
            "glm-1".to_string(),
            AccountRuntime {
                error: Some("网络错误，展示缓存数据".to_string()),
                last_quota: Some((glm_quota(0.0, 0.0), 1_899_000_000)),
                ..AccountRuntime::default()
            },
        );
        let panel = assemble_panel_state_with(&inner, &settings, &|_| true);
        assert!(!panel.accounts[0].low_warning, "失败账号不算低额");
        assert!(!any_low_warning(&panel), "托盘不应变红");
        // 缓存配额照常展示（错误横幅与数据并存）
        assert!(panel.accounts[0].quota.is_some());
        assert_eq!(
            panel.accounts[0].error.as_deref(),
            Some("网络错误，展示缓存数据")
        );
    }

    #[test]
    fn tooltip_covers_glm_quota() {
        // GLM 账号走 Kimi 同款窗口摘要（7天/5h 剩余百分比）
        let glm = glm_account("glm-1", "GLM 号");
        let settings = settings_with_accounts(vec![glm], 20.0);
        let mut inner = Inner::default();
        inner.accounts.insert(
            "glm-1".to_string(),
            AccountRuntime {
                error: None,
                last_quota: Some((glm_quota(36.0, 87.0), 1)),
                ..AccountRuntime::default()
            },
        );
        let panel = assemble_panel_state_with(&inner, &settings, &|_| true);
        let tip = worst_account_tooltip(&panel, i18n::Lang::Zh).unwrap();
        assert_eq!(tip, "\n7天剩余 87% · 5h剩余 36%", "实际: {tip}");
    }
}
