// 前端 ↔ 后端 IPC 封装。
// 命令名为架构方钉死的契约：get_panel_state / refresh_now / open_settings；
// 后端状态变化时广播事件 quota-updated（payload 为 PanelState JSON）。
// 纯浏览器开发（无 window.__TAURI_INTERNALS__）时走内置 mock，便于脱离后端独立开发。

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import type {
  AppSettings,
  CredentialStatus,
  DeviceLoginState,
  HistoryPoint,
  MonthlyInfo,
  PanelState,
  UpdateInfo,
} from "./types";

/** 是否在 Tauri 运行时内（纯浏览器 vite dev 时为 false，走 mock 数据）。
 *  注意：Tauri v2 只在 withGlobalTauri=true 时注入 window.__TAURI__，
 *  恒定存在的是 __TAURI_INTERNALS__（IPC 初始化脚本注入），探测必须用它。 */
export const isTauri = "__TAURI_INTERNALS__" in window;

// mock 的重置时间写成相对当前时间的未来值，倒计时展示更真实
const MOCK_WEEKLY_RESET = new Date(Date.now() + 3 * 24 * 3600 * 1000).toISOString();
const MOCK_FIVE_HOUR_RESET = new Date(Date.now() + 2 * 3600 * 1000).toISOString();
const MOCK_MONTHLY_RESET = new Date(Date.now() + 12 * 24 * 3600 * 1000).toISOString();

/** 浏览器 mock 的月度总量假数据：总 15.07%（Kimi 11.12% + Code 3.95%） */
const MOCK_MONTHLY: MonthlyInfo = {
  total_pct: 15.07,
  kimi_pct: 11.12,
  code_pct: 3.95,
  reset_time: MOCK_MONTHLY_RESET,
};

/** 浏览器开发用的 mock 面板状态：weekly 87% / five_hour 36% / 中级版 / Booster 未开通 */
const MOCK_STATE: PanelState = {
  credential: true,
  loading: false,
  quota: {
    weekly: {
      used: 130,
      limit: 1000,
      remaining: 870,
      reset_time: MOCK_WEEKLY_RESET,
      percent_remaining: 87,
    },
    five_hour: {
      used: 64,
      limit: 100,
      remaining: 36,
      reset_time: MOCK_FIVE_HOUR_RESET,
      percent_remaining: 36,
    },
    membership_level: "LEVEL_INTERMEDIATE",
    booster: {
      enabled: false,
      balance_yuan: 0,
      monthly_charge_limit_enabled: false,
    },
  },
  fetched_at: Math.floor(Date.now() / 1000),
  error: null,
  low_warning: false,
  // 月度卡也在浏览器 mock 下展示，便于脱离后端调试分段条
  monthly: MOCK_MONTHLY,
  monthly_error: null,
};

/** 获取面板状态（含缓存配额，断网也可立即渲染） */
export async function getPanelState(): Promise<PanelState> {
  if (!isTauri) return MOCK_STATE;
  return invoke<PanelState>("get_panel_state");
}

/** 立即刷新一次配额，返回最新面板状态 */
export async function refreshNow(): Promise<PanelState> {
  if (!isTauri) {
    // 模拟网络延迟，便于调试加载态
    await new Promise((resolve) => setTimeout(resolve, 500));
    return MOCK_STATE;
  }
  return invoke<PanelState>("refresh_now");
}

/** 打开设置窗口 */
export async function openSettings(): Promise<void> {
  if (!isTauri) {
    console.info("[mock] open_settings");
    return;
  }
  return invoke<void>("open_settings");
}

/**
 * 订阅后端推送的面板状态更新（quota-updated 事件）。
 * 返回反注册函数，供组件卸载时调用。
 */
export function onQuotaUpdated(cb: (state: PanelState) => void): () => void {
  if (!isTauri) {
    // 浏览器 mock 没有后端推送，返回空的反注册函数
    return () => {};
  }
  let unlisten: (() => void) | null = null;
  // listen 异步完成注册；若组件在注册完成前卸载，用 cancelled 标记兜底反注册
  let cancelled = false;
  listen<PanelState>("quota-updated", (event) => cb(event.payload)).then((fn) => {
    if (cancelled) fn();
    else unlisten = fn;
  });
  return () => {
    cancelled = true;
    unlisten?.();
  };
}

// ============ 用量趋势（本地历史采样，纯事实不预测）============

/**
 * 获取本地累积的历史采样点（每次成功刷新记录一条，百分比为"已用"语义）。
 * 浏览器 mock 生成近 24 小时、每 10 分钟一个点的合成数据：
 * weekly 从 20 缓慢爬升到 65，five_hour 呈锯齿波（模拟 5 小时窗口到期重置），
 * monthly 从 14 缓升到 15。
 */
export async function getUsageHistory(): Promise<HistoryPoint[]> {
  if (!isTauri) {
    const nowSec = Math.floor(Date.now() / 1000);
    const points: HistoryPoint[] = [];
    // 24 小时 × 每小时 6 个点 = 144 个间隔，含首尾共 145 个点
    for (let i = 0; i <= 144; i++) {
      const ratio = i / 144;
      // 5 小时窗口 = 30 个点一个周期：周期内爬升、到顶骤降回起点（锯齿）
      const phase = (i % 30) / 30;
      points.push({
        t: nowSec - 24 * 3600 + i * 600,
        weekly: Math.min(100, 20 + 45 * ratio + Math.sin(i / 7) * 1.5),
        five_hour: Math.min(100, 8 + 80 * phase + Math.sin(i / 5) * 2),
        monthly: 14 + ratio,
      });
    }
    return points;
  }
  return invoke<HistoryPoint[]>("get_usage_history");
}

// ============ 第 5 步：设置与凭证 ============

/** 浏览器 mock 的可变"数据库"：让设置页在纯浏览器下也能走完整交互 */
const mockDb = {
  settings: {
    login_method: null,
    refresh_interval_min: 5,
    low_warn_enabled: true,
    warn_threshold_pct: 20,
    autostart: false,
    hotkey: null,
    language: "system",
  } as AppSettings,
  // 预置一个假 Key，方便浏览器开发时看到"已配置"徽标
  apiKey: "sk-kimi-mock9f8e7d6c5b4a" as string | null,
  oauthConfigured: false,
  // 网页 token（月度总量用）：初始未配置，走 setWebToken/clearWebToken 变更
  webToken: null as string | null,
};

/** 浏览器 mock 的 device-login-updated 事件订阅者集合 */
const mockDeviceLoginListeners = new Set<(state: DeviceLoginState) => void>();
/** 浏览器 mock 的"授权成功"定时器（取消登录时清除） */
let mockDeviceLoginTimer: ReturnType<typeof setTimeout> | null = null;

/** mock 广播设备登录状态 */
function emitMockDeviceLogin(state: DeviceLoginState) {
  mockDeviceLoginListeners.forEach((cb) => cb(state));
}

/** mock 的 idle 状态（各字段清零） */
const MOCK_IDLE_LOGIN: DeviceLoginState = {
  status: "idle",
  user_code: null,
  verification_uri: null,
  verification_uri_complete: null,
  expires_in: null,
  error: null,
};

/** mock 的 waiting 状态：固定授权码 + 5 分钟有效期 */
function mockWaitingLogin(): DeviceLoginState {
  return {
    status: "waiting",
    user_code: "ABCD-EFGH",
    verification_uri: "https://www.kimi.com/oauth/device",
    verification_uri_complete: "https://www.kimi.com/oauth/device?user_code=ABCD-EFGH",
    expires_in: 300,
    error: null,
  };
}

/** 由 mock 数据库推导 CredentialStatus */
function mockCredentialStatus(): CredentialStatus {
  return {
    login_method: mockDb.settings.login_method,
    api_key_configured: mockDb.apiKey !== null,
    api_key_masked: mockDb.apiKey ? `sk-kimi-****…${mockDb.apiKey.slice(-4)}` : null,
    oauth_configured: mockDb.oauthConfigured,
    web_token_configured: mockDb.webToken !== null,
  };
}

/** 获取应用设置 */
export async function getSettings(): Promise<AppSettings> {
  if (!isTauri) return { ...mockDb.settings };
  return invoke<AppSettings>("get_settings");
}

/** 保存应用设置（整体替换，含登录方式与通用项） */
export async function saveSettings(settings: AppSettings): Promise<void> {
  if (!isTauri) {
    mockDb.settings = { ...settings };
    return;
  }
  return invoke<void>("save_settings", { settings });
}

/**
 * 订阅后端广播的设置变更（settings-changed 事件，payload 为 AppSettings；
 * save_settings 成功后触发，两个窗口据此即时切换语言）。
 * 返回反注册函数，供组件卸载时调用。
 */
export function onSettingsChanged(cb: (settings: AppSettings) => void): () => void {
  if (!isTauri) {
    // 浏览器 mock 没有跨窗口广播，返回空的反注册函数
    return () => {};
  }
  let unlisten: (() => void) | null = null;
  // 与 onQuotaUpdated 相同的兜底：注册完成前卸载也能正确反注册
  let cancelled = false;
  listen<AppSettings>("settings-changed", (event) => cb(event.payload)).then((fn) => {
    if (cancelled) fn();
    else unlisten = fn;
  });
  return () => {
    cancelled = true;
    unlisten?.();
  };
}

/** 保存 API Key（写入系统钥匙串）；后端校验失败会抛中文错误，原样透传 */
export async function setApiKey(key: string): Promise<void> {
  if (!isTauri) {
    const k = key.trim();
    // mock 复刻后端格式校验，便于浏览器下调试错误提示
    if (!k.startsWith("sk-kimi-")) {
      throw new Error("API Key 格式不正确：应以 sk-kimi- 开头");
    }
    if (k.length < 12) {
      throw new Error("API Key 格式不正确：长度过短");
    }
    mockDb.apiKey = k;
    return;
  }
  return invoke<void>("set_api_key", { key });
}

/** 清除已保存的 API Key */
export async function clearApiKey(): Promise<void> {
  if (!isTauri) {
    mockDb.apiKey = null;
    return;
  }
  return invoke<void>("clear_api_key");
}

/** 获取凭证配置状态（脱敏 Key + 各方式是否已配置） */
export async function getCredentialStatus(): Promise<CredentialStatus> {
  if (!isTauri) return mockCredentialStatus();
  return invoke<CredentialStatus>("get_credential_status");
}

// ============ 月度总量（网页 token，可选增强）============

/**
 * 校验并保存网页 token（kimi-auth cookie 的值，支持整串 cookie 粘贴由后端识别）。
 * 后端先调网页接口校验，失败抛中文错误原样透传；成功返回当月月度总量。
 */
export async function setWebToken(token: string): Promise<MonthlyInfo> {
  if (!isTauri) {
    if (token.trim() === "") throw new Error("请粘贴 kimi-auth 的值");
    // 模拟网络延迟，便于调试"校验中…"加载态
    await new Promise((resolve) => setTimeout(resolve, 600));
    mockDb.webToken = token.trim();
    return MOCK_MONTHLY;
  }
  return invoke<MonthlyInfo>("set_web_token", { token });
}

/** 清除已保存的网页 token（面板月度卡随之不再展示） */
export async function clearWebToken(): Promise<void> {
  if (!isTauri) {
    mockDb.webToken = null;
    return;
  }
  return invoke<void>("clear_web_token");
}

/** 发起 OAuth 设备码登录：返回 waiting 状态，后续进度由 device-login-updated 事件推送 */
export async function startDeviceLogin(): Promise<DeviceLoginState> {
  if (!isTauri) {
    if (mockDeviceLoginTimer !== null) clearTimeout(mockDeviceLoginTimer);
    // 模拟用户 8 秒后完成浏览器授权
    mockDeviceLoginTimer = setTimeout(() => {
      mockDeviceLoginTimer = null;
      mockDb.oauthConfigured = true;
      emitMockDeviceLogin({ ...MOCK_IDLE_LOGIN, status: "success" });
    }, 8000);
    return mockWaitingLogin();
  }
  return invoke<DeviceLoginState>("start_device_login");
}

/** 取消进行中的设备码登录（无进行中流程时为空操作） */
export async function cancelDeviceLogin(): Promise<void> {
  if (!isTauri) {
    if (mockDeviceLoginTimer !== null) {
      clearTimeout(mockDeviceLoginTimer);
      mockDeviceLoginTimer = null;
    }
    emitMockDeviceLogin(MOCK_IDLE_LOGIN);
    return;
  }
  return invoke<void>("cancel_device_login");
}

/** 退出 OAuth 账号授权登录（清除本地 token） */
export async function oauthLogout(): Promise<void> {
  if (!isTauri) {
    mockDb.oauthConfigured = false;
    return;
  }
  return invoke<void>("oauth_logout");
}

/**
 * 订阅设备码登录进度（device-login-updated 事件，payload 为 DeviceLoginState）。
 * 返回反注册函数，供组件卸载时调用。
 */
export function onDeviceLoginUpdated(cb: (state: DeviceLoginState) => void): () => void {
  if (!isTauri) {
    mockDeviceLoginListeners.add(cb);
    return () => {
      mockDeviceLoginListeners.delete(cb);
    };
  }
  let unlisten: (() => void) | null = null;
  // 与 onQuotaUpdated 相同的兜底：注册完成前卸载也能正确反注册
  let cancelled = false;
  listen<DeviceLoginState>("device-login-updated", (event) => cb(event.payload)).then((fn) => {
    if (cancelled) fn();
    else unlisten = fn;
  });
  return () => {
    cancelled = true;
    unlisten?.();
  };
}

/** 在系统浏览器中打开外部链接（浏览器 dev 时退化为 window.open） */
export async function openExternalUrl(url: string): Promise<void> {
  if (!isTauri) {
    window.open(url, "_blank");
    return;
  }
  return openUrl(url);
}

// ============ 第 6 步：更新检查 ============

/**
 * 检查应用更新。force=true 强制走网络（设置页手动点击）；缺省走后端时间缓存
 * （上次成功 6 小时 / 上次错误 10 分钟内复用旧结果）。
 * 检查失败时后端不抛错，而是把原因放进返回的 error 字段，调用方按需静默
 */
export async function checkUpdate(force?: boolean): Promise<UpdateInfo> {
  if (!isTauri) {
    // mock 固定返回"有新版"，便于浏览器下查看徽标/提示效果
    return {
      current: "0.1.0",
      latest: "0.2.0",
      has_update: true,
      release_url: "https://example.com",
      error: null,
    };
  }
  return invoke<UpdateInfo>("check_update", { force });
}

/**
 * 订阅后端推送的更新检查结果（update-info 事件，payload 为 UpdateInfo；
 * 面板打开时的后台检查、强制检查完成时广播）。
 * 返回反注册函数，供组件卸载时调用。
 */
export function onUpdateInfo(cb: (info: UpdateInfo) => void): () => void {
  if (!isTauri) {
    // 浏览器 mock 没有后端推送，返回空的反注册函数
    return () => {};
  }
  let unlisten: (() => void) | null = null;
  // 与 onQuotaUpdated 相同的兜底：注册完成前卸载也能正确反注册
  let cancelled = false;
  listen<UpdateInfo>("update-info", (event) => cb(event.payload)).then((fn) => {
    if (cancelled) fn();
    else unlisten = fn;
  });
  return () => {
    cancelled = true;
    unlisten?.();
  };
}

// ============ 诊断与日志 ============

/** 打开日志目录（浏览器 mock 仅打印） */
export async function openLogDir(): Promise<void> {
  if (!isTauri) {
    console.info("[mock] open_log_dir");
    return;
  }
  return invoke<void>("open_log_dir");
}

/**
 * 导出诊断文件：后端写入诊断文本并自动打开所在目录，返回文件路径。
 * 失败抛中文错误原样透传；浏览器 mock 返回固定假路径
 */
export async function exportDiagnostics(): Promise<string> {
  if (!isTauri) {
    return "C:\\Users\\demo\\AppData\\Roaming\\KimiCodeBar\\diagnostics-20260726-120000.txt";
  }
  return invoke<string>("export_diagnostics");
}
