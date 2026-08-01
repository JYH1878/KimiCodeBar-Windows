// 前后端共享类型契约 —— 与 Rust 侧 serde 输出（snake_case）一一对应。
// 架构方钉死，前端/后端代理均不得修改字段名；如需扩展需双方同步。

/** 单个时间窗口（7 天 / 5 小时）用量，百分比为"剩余"语义 */
export interface QuotaDetail {
  used: number;
  limit: number;
  remaining: number;
  /** RFC3339，如 2026-07-27T04:56:27.987852Z；可能缺失 */
  reset_time?: string;
  /** 剩余百分比 0-100 */
  percent_remaining: number;
}

export interface TotalQuotaInfo {
  limit: number;
  remaining: number;
  percent_remaining: number;
}

export interface BoosterInfo {
  enabled: boolean;
  balance_yuan: number;
  monthly_charge_limit_enabled: boolean;
  monthly_charge_limit_yuan?: number;
  monthly_used_yuan?: number;
  topup_limit_yuan?: number;
}

export interface KimiQuota {
  weekly?: QuotaDetail;
  five_hour?: QuotaDetail;
  total?: TotalQuotaInfo;
  /** LEVEL_FREE / LEVEL_BASIC / LEVEL_INTERMEDIATE / LEVEL_ADVANCED */
  membership_level?: string;
  booster?: BoosterInfo;
}

/** 面板状态：get_panel_state / refresh_now 的返回，quota-updated 事件的 payload */
export interface PanelState {
  /** 是否已配置任一凭证（API Key 或 OAuth） */
  credential: boolean;
  /** 是否正在后台刷新 */
  loading: boolean;
  /** 最近一次成功的配额（可能来自缓存；断网时依然展示） */
  quota: KimiQuota | null;
  /** 上次成功刷新时间（epoch 秒） */
  fetched_at: number | null;
  /** 最近一次错误信息（与缓存并存，用于非阻断横幅） */
  error: string | null;
  /** 任一窗口剩余低于阈值（默认 20%），UI 标红 */
  low_warning: boolean;
  /** 月度总量（已配置网页 token 且有数据时展示；可能为 null） */
  monthly?: MonthlyInfo | null;
  /** 月度数据获取失败原因（如网页登录态过期）；成功为 null */
  monthly_error?: string | null;
}

// ============ 第 5 步：设置与凭证契约 ============

/** 登录方式：A=手动 API Key，B=OAuth 设备码授权 */
export type LoginMethod = "api_key" | "oauth";

/** 应用设置：get_settings / save_settings 的载荷（与 Rust Settings 一致） */
export interface AppSettings {
  /** 未设置时为 null，后端自动优先 api_key 其次 oauth */
  login_method: LoginMethod | null;
  /** 自动刷新间隔（分钟，最小 1，默认 5） */
  refresh_interval_min: number;
  /** 低额度告警开关（默认 true） */
  low_warn_enabled: boolean;
  /** 告警阈值百分比（默认 20） */
  warn_threshold_pct: number;
  /** 开机自启（默认 false，保存时同步注册表） */
  autostart: boolean;
  /** 全局热键（如 "Ctrl+Shift+K"），null/空串 = 禁用；保存时后端重新注册 */
  hotkey?: string | null;
  /** 界面语言："system"（跟随系统）/ "zh" / "en"，默认 system */
  language?: string | null;
  /** 主题模式："system" / "dark" / "light"，默认 system */
  theme?: ThemeMode | null;
  /** 面板背景图片文件名（后端配置目录内），null = 无自定义背景 */
  background_image?: string | null;
  /** 预设背景 id（night / aurora / violet / ember），null = 未选；生效时优先于 background_image */
  background_preset?: string | null;
}

/** 凭证配置状态：get_credential_status 的返回 */
export interface CredentialStatus {
  /** 当前生效的登录方式（settings.login_method） */
  login_method: LoginMethod | null;
  api_key_configured: boolean;
  /** 脱敏展示，如 sk-kimi-****…a4nr；未配置为 null */
  api_key_masked: string | null;
  oauth_configured: boolean;
  /** 网页 token（月度总量用）是否已配置 */
  web_token_configured: boolean;
}

// ============ 用量趋势（本地历史，纯事实不预测）============

/** 历史采样点：每次成功刷新记录一条，百分比为"已用"语义 */
export interface HistoryPoint {
  /** epoch 秒 */
  t: number;
  /** 7 天窗口已用百分比（缺失为 null） */
  weekly?: number | null;
  /** 5 小时窗口已用百分比（缺失为 null） */
  five_hour?: number | null;
  /** 月度总量已用百分比（缺失为 null） */
  monthly?: number | null;
}

// ============ 本地 Token 消耗统计（扫描 wire.jsonl，不依赖 API）============

/** 某一天的消耗 */
export interface DailyUsage {
  /** 本地日期 YYYY-MM-DD */
  date: string;
  tokens: number;
}

/** 某模型的累计消耗 */
export interface ModelUsage {
  model: string;
  tokens: number;
}

/** get_local_usage 的返回：本地 token 消耗统计 */
export interface LocalUsageStats {
  /** 今日总消耗 */
  today_tokens: number;
  /** 昨日总消耗 */
  yesterday_tokens: number;
  /** 最近 7 天逐日消耗（升序） */
  daily: DailyUsage[];
  /** 按模型累计（今日，降序 top 5） */
  by_model: ModelUsage[];
  /** 上次扫描时间（epoch 秒），未扫过为 null */
  last_scan_at: number | null;
}

// ============ 主题 ============

/** 主题模式："system"（跟随系统）/ "dark" / "light"，默认 system */
export type ThemeMode = "system" | "dark" | "light";

// ============ 月度总量（网页 token，可选增强）============

/** 月度总量：来自网页端 GetSubscriptionStats，百分比为"已用"语义 */
export interface MonthlyInfo {
  /** 月度总已用百分比（Kimi + Code 合计） */
  total_pct: number;
  /** 其中 Kimi 已用百分比（= total - code 防御计算） */
  kimi_pct: number;
  /** 其中 Code 已用百分比 */
  code_pct: number;
  /** 月度重置时间 RFC3339；可能缺失 */
  reset_time?: string;
}

/** 设备码登录流程状态：start_device_login 的返回 + device-login-updated 事件 payload */
export interface DeviceLoginState {
  /** idle=未开始/已取消，waiting=等待用户授权，success=已拿到 token，error=失败 */
  status: "idle" | "waiting" | "success" | "error";
  /** 展示给用户的授权码（waiting 时有值） */
  user_code: string | null;
  verification_uri: string | null;
  /** 含码直达链接，点"打开浏览器"用 */
  verification_uri_complete: string | null;
  /** 设备码有效期（秒） */
  expires_in: number | null;
  /** status=error 时的错误信息 */
  error: string | null;
}

// ============ 第 6 步：更新检查契约 ============

/** check_update 的返回。后端时间缓存：上次成功 6 小时 / 上次错误 10 分钟内复用旧结果，force 参数可强制走网络 */
export interface UpdateInfo {
  /** 当前版本，如 0.1.0 */
  current: string;
  /** 远端最新版本（检查失败为 null） */
  latest: string | null;
  /** 是否有新版本 */
  has_update: boolean;
  /** 有新版本时的 Release 页面地址（点击去下载） */
  release_url: string | null;
  /** 检查失败原因（网络等）；成功为 null */
  error: string | null;
}
