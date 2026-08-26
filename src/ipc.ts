// 前端 ↔ 后端 IPC 封装。
// 命令名为架构方钉死的契约：get_panel_state / refresh_now / open_settings；
// 后端状态变化时广播事件 quota-updated（payload 为 PanelState JSON）。
// 纯浏览器开发（无 window.__TAURI_INTERNALS__）时走内置 mock，便于脱离后端独立开发。

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import type {
  Account,
  AccountProvider,
  AppSettings,
  CredentialStatus,
  DailyUsage,
  DeviceLoginState,
  HistoryPoint,
  LocalUsageStats,
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

/** 浏览器 mock 的账号列表（两个 Kimi 账号 + 一个 DeepSeek 账号 + 一个 GLM 账号，便于调试翻页与各提供商页） */
const MOCK_ACCOUNTS: Account[] = [
  { id: "mock-acc-1", name: "账号 1", login_method: "api_key", provider: "kimi" },
  { id: "mock-acc-2", name: "演示号", login_method: "oauth", provider: "kimi" },
  { id: "mock-acc-3", name: "DeepSeek 演示", login_method: "api_key", provider: "deepseek" },
  { id: "mock-acc-4", name: "GLM 演示", login_method: "api_key", provider: "glm" },
];

/** 浏览器 mock 的 DeepSeek 余额假数据（标注：仅浏览器 dev 用，真实数据来自接口/缓存） */
const MOCK_DEEPSEEK_BALANCE = {
  is_available: true,
  currency: "CNY",
  total_balance: 12.34,
  granted_balance: 2.0,
  topped_up_balance: 10.34,
};

/** 浏览器开发用的 mock 面板状态：账号 1 健康（weekly 87% / five_hour 36% / 中级版），
 *  演示号模拟"无效 token"——缓存数据照显 + 错误横幅，托盘不变红 */
const MOCK_STATE: PanelState = {
  loading: false,
  accounts: [
    {
      account: MOCK_ACCOUNTS[0],
      credential: true,
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
    },
    {
      account: MOCK_ACCOUNTS[1],
      credential: true,
      quota: {
        weekly: {
          used: 950,
          limit: 1000,
          remaining: 50,
          reset_time: MOCK_WEEKLY_RESET,
          percent_remaining: 5,
        },
      },
      fetched_at: Math.floor(Date.now() / 1000) - 3600,
      error: "凭证无效或已过期，请在设置中重新配置",
      // 拉取失败的账号不算低额（GOAL 拍板）：缓存剩 5% 也不标红
      low_warning: false,
      monthly_error: "月度数据刷新失败",
    },
    {
      account: MOCK_ACCOUNTS[2],
      credential: true,
      // DeepSeek 账号：Kimi 字段为空，余额挂在 deepseek_balance
      quota: null,
      fetched_at: Math.floor(Date.now() / 1000),
      error: null,
      low_warning: false,
      deepseek_balance: MOCK_DEEPSEEK_BALANCE,
    },
    {
      account: MOCK_ACCOUNTS[3],
      credential: true,
      // GLM 账号：额度映射进 KimiQuota 契约（以 100 为总量合成的百分比口径），无月度/Booster
      quota: {
        weekly: {
          used: 61,
          limit: 100,
          remaining: 39,
          reset_time: MOCK_WEEKLY_RESET,
          percent_remaining: 39,
        },
        five_hour: {
          used: 42.5,
          limit: 100,
          remaining: 57.5,
          reset_time: MOCK_FIVE_HOUR_RESET,
          percent_remaining: 57.5,
        },
        membership_level: "pro",
      },
      fetched_at: Math.floor(Date.now() / 1000),
      error: null,
      low_warning: false,
    },
  ],
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

/** 打开设置窗口；section 指定定位分区（如 "account-add" 定位到账号添加表单） */
export async function openSettings(section?: string): Promise<void> {
  if (!isTauri) {
    console.info(`[mock] open_settings section=${section ?? ""}`);
    return;
  }
  return invoke<void>("open_settings", { section: section ?? null });
}

/**
 * 订阅设置页定位请求（settings-navigate 事件，payload 为分区名；
 * 面板「+」页等入口借此让设置页定位到指定表单）。
 * 返回反注册函数，供组件卸载时调用。
 */
export function onSettingsNavigate(cb: (section: string) => void): () => void {
  if (!isTauri) {
    return () => {};
  }
  let unlisten: (() => void) | null = null;
  let cancelled = false;
  listen<string>("settings-navigate", (event) => cb(event.payload)).then((fn) => {
    if (cancelled) fn();
    else unlisten = fn;
  });
  return () => {
    cancelled = true;
    unlisten?.();
  };
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
export async function getUsageHistory(accountId: string): Promise<HistoryPoint[]> {
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
  return invoke<HistoryPoint[]>("get_usage_history", { accountId });
}

// ============ 本地 Token 消耗统计（扫描 wire.jsonl，不依赖 API）============

/**
 * 浏览器 mock 的本地 token 统计：按账号返回明显不同的三份数字（归属联调用，
 * 翻两页数字必须不同；DeepSeek 账号页也显示此卡故同样有一份）。
 * daily 日期按本地时区生成（与后端 YYYY-MM-DD 契约一致），末位即今日。
 */
const MOCK_LOCAL_USAGE: Record<string, { today: number; yesterday: number; amounts: number[]; byModel: { model: string; tokens: number }[] }> = {
  "mock-acc-1": {
    today: 128400,
    yesterday: 96200,
    amounts: [42300, 58700, 31200, 88900, 76400, 96200, 128400],
    // kimi-code/kimi-for-coding 演示 K2.7 展示名映射
    byModel: [
      { model: "kimi-code/k3", tokens: 406000 },
      { model: "kimi-code/kimi-for-coding", tokens: 128000 },
    ],
  },
  "mock-acc-2": {
    today: 21500,
    yesterday: 47800,
    amounts: [66000, 51200, 73400, 28900, 41500, 47800, 21500],
    byModel: [{ model: "kimi-code/k3-256k", tokens: 21500 }],
  },
  "mock-acc-3": {
    today: 8300,
    yesterday: 12600,
    amounts: [9800, 15200, 7400, 21000, 16900, 12600, 8300],
    byModel: [{ model: "deepseek-v4-flash", tokens: 8300 }],
  },
};

/** 由 mock 配置生成一份 LocalUsageStats（amounts 末位即今日；last_event_at 演示"当前活跃"） */
function mockLocalUsage(accountId: string): LocalUsageStats {
  const pad = (n: number) => String(n).padStart(2, "0");
  const mock = MOCK_LOCAL_USAGE[accountId] ?? { today: 0, yesterday: 0, amounts: [0, 0, 0, 0, 0, 0, 0], byModel: [] };
  const daily: DailyUsage[] = mock.amounts.map((tokens, i) => {
    const d = new Date(Date.now() - (6 - i) * 24 * 3600 * 1000);
    return {
      date: `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`,
      tokens,
    };
  });
  return {
    today_tokens: mock.today,
    yesterday_tokens: mock.yesterday,
    daily,
    by_model: mock.byModel,
    last_scan_at: Math.floor(Date.now() / 1000),
    last_event_at: Date.now(),
  };
}

/** 获取本地 token 消耗统计（按账号归属：后端扫描 wire.jsonl 按 CLI 凭证归到各账号） */
export async function getLocalUsage(accountId: string): Promise<LocalUsageStats> {
  if (!isTauri) return mockLocalUsage(accountId);
  return invoke<LocalUsageStats>("get_local_usage", { accountId });
}

/**
 * 导出用量报告：后端汇总本地统计与历史采样写入文件并自动打开所在目录，返回目录路径。
 * 失败抛中文错误原样透传；浏览器 mock 返回固定假路径
 */
export async function exportUsageReport(): Promise<string> {
  if (!isTauri) {
    return "C:\\Users\\demo\\AppData\\Roaming\\KimiCodeBar\\usage-report";
  }
  return invoke<string>("export_usage_report");
}

// ============ 第 5 步：设置与凭证 ============

/** 浏览器 mock 的可变"数据库"：让设置页在纯浏览器下也能走完整交互 */
const mockDb = {
  settings: {
    refresh_interval_min: 5,
    adaptive_refresh: true,
    low_warn_enabled: true,
    warn_threshold_pct: 20,
    deepseek_warn_threshold: 5,
    autostart: false,
    minimal_mode: false,
    statusline_enabled: false,
    hotkey: null,
    language: "system",
    theme: "system",
    background_image: null,
    background_preset: null,
  } as AppSettings,
  // 账号列表（顺序 = 面板页顺序）；初始与面板 mock 一致
  accounts: MOCK_ACCOUNTS.map((a) => ({ ...a })),
  // 各账号的凭证（按账号 id 索引）：预置账号 1 一个假 Key，方便看到"已配置"徽标
  apiKeys: { "mock-acc-1": "sk-kimi-mock9f8e7d6c5b4a" } as Record<string, string>,
  // 各账号已登记的额外 API Key（本地消耗归属用）：预置账号 1 一把，便于调试列表展示
  apiKeysExtra: { "mock-acc-1": ["sk-kimi-extra5f6e7d8c9b0a"] } as Record<string, string[]>,
  oauthAccounts: new Set<string>(),
  // 网页凭证（refresh_token / 旧 kimi-auth，月度总量用）：初始未配置
  webTokens: {} as Record<string, string>,
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

/** 由 mock 数据库推导该账号的 CredentialStatus */
function mockCredentialStatus(accountId: string): CredentialStatus {
  const account = mockDb.accounts.find((a) => a.id === accountId);
  const apiKey = mockDb.apiKeys[accountId] ?? null;
  return {
    login_method: account?.login_method ?? null,
    api_key_configured: apiKey !== null,
    api_key_masked: apiKey ? `sk-kimi-****…${apiKey.slice(-4)}` : null,
    // 与后端 mask_api_key 同规则（>12 字符：前 8 + … + 后 4，否则全显），按登记顺序
    api_key_extra_masked: (mockDb.apiKeysExtra[accountId] ?? []).map(mockMaskApiKey),
    oauth_configured: mockDb.oauthAccounts.has(accountId),
    web_token_configured: mockDb.webTokens[accountId] != null,
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

/** 录制全局热键前暂停全局热键注册（否则已注册的组合被系统拦截，录制框收不到按键） */
export async function pauseGlobalHotkey(): Promise<void> {
  if (!isTauri) {
    console.info("[mock] pause_global_hotkey");
    return;
  }
  return invoke<void>("pause_global_hotkey");
}

/** 录制结束后按已保存设置恢复全局热键（未保存的录制值不生效） */
export async function resumeGlobalHotkey(): Promise<void> {
  if (!isTauri) {
    console.info("[mock] resume_global_hotkey");
    return;
  }
  return invoke<void>("resume_global_hotkey");
}

// ============ 面板背景图片（静态图，PNG/JPG/WebP ≤10MB）============
// 供图走后端 kimibg:// 自定义协议（面板用 convertFileSrc 取 URL），不经 IPC 搬字节

/**
 * 上传面板背景图（base64，不含 "data:..." 前缀）。
 * 后端嗅探格式（GIF 拒绝）与大小（≤10MB），失败抛中文错误原样透传
 */
export async function setBackgroundImage(dataBase64: string): Promise<void> {
  if (!isTauri) {
    mockDb.settings.background_image = "background.png";
    mockDb.settings.background_preset = null;
    return;
  }
  return invoke<void>("set_background_image", { dataBase64 });
}

/** 清除面板背景（图片文件与预设都清掉，= 无背景；未设置过为空操作） */
export async function clearBackgroundImage(): Promise<void> {
  if (!isTauri) {
    mockDb.settings.background_image = null;
    mockDb.settings.background_preset = null;
    return;
  }
  return invoke<void>("clear_background_image");
}

/**
 * 选择预设背景（id 限 night / aurora / violet / ember；null = 取消预设，切回自定义图/无背景）。
 * 不影响已上传的图片文件，失败抛中文错误原样透传
 */
export async function setBackgroundPreset(preset: string | null): Promise<void> {
  if (!isTauri) {
    mockDb.settings.background_preset = preset;
    return;
  }
  return invoke<void>("set_background_preset", { preset });
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

// ============ 账号管理 ============

/** 账号列表（顺序 = 面板页顺序） */
export async function listAccounts(): Promise<Account[]> {
  if (!isTauri) return mockDb.accounts.map((a) => ({ ...a }));
  return invoke<Account[]>("list_accounts");
}

/** 新增账号（全部提供商合计超上限 10 个报错；名称为空默认「账号 N」），返回新建账号 */
export async function addAccount(name?: string, provider: AccountProvider = "kimi"): Promise<Account> {
  if (!isTauri) {
    if (mockDb.accounts.length >= 10) throw new Error("最多支持 10 个账号");
    const trimmed = name?.trim();
    const account: Account = {
      id: `mock-acc-${Date.now()}`,
      name: trimmed || `账号 ${mockDb.accounts.length + 1}`,
      login_method: null,
      provider,
    };
    mockDb.accounts.push(account);
    return { ...account };
  }
  return invoke<Account>("add_account", { name: name ?? null, provider });
}

/** 账号改名（空名 / 不存在报错） */
export async function renameAccount(accountId: string, name: string): Promise<void> {
  if (!isTauri) {
    const account = mockDb.accounts.find((a) => a.id === accountId);
    if (!account) throw new Error("账号不存在");
    const trimmed = name.trim();
    if (!trimmed) throw new Error("账号名称不能为空");
    account.name = trimmed;
    return;
  }
  return invoke<void>("rename_account", { accountId, name });
}

/** 账号上移/下移（direction 取 -1 / +1；越界为无操作） */
export async function moveAccount(accountId: string, direction: number): Promise<void> {
  if (!isTauri) {
    const i = mockDb.accounts.findIndex((a) => a.id === accountId);
    const j = i + Math.sign(direction);
    if (i < 0 || j < 0 || j >= mockDb.accounts.length) return;
    [mockDb.accounts[i], mockDb.accounts[j]] = [mockDb.accounts[j], mockDb.accounts[i]];
    return;
  }
  return invoke<void>("move_account", { accountId, direction });
}

/** 删除账号（连同该账号全部本地数据；调用方需先做二次确认） */
export async function deleteAccount(accountId: string): Promise<void> {
  if (!isTauri) {
    mockDb.accounts = mockDb.accounts.filter((a) => a.id !== accountId);
    delete mockDb.apiKeys[accountId];
    delete mockDb.apiKeysExtra[accountId];
    delete mockDb.webTokens[accountId];
    mockDb.oauthAccounts.delete(accountId);
    return;
  }
  return invoke<void>("delete_account", { accountId });
}

/** 切换该账号的登录方式（"api_key" / "oauth"；null 为未显式选择） */
export async function setAccountLoginMethod(
  accountId: string,
  method: "api_key" | "oauth" | null,
): Promise<void> {
  if (!isTauri) {
    const account = mockDb.accounts.find((a) => a.id === accountId);
    if (account) account.login_method = method;
    return;
  }
  return invoke<void>("set_account_login_method", { accountId, method });
}

/** 保存该账号的 API Key（写入系统凭据管理器）；后端校验失败会抛中文错误，原样透传 */
export async function setApiKey(accountId: string, key: string): Promise<void> {
  if (!isTauri) {
    const k = key.trim();
    // mock 复刻后端格式校验（按提供商分派：DeepSeek 查 sk- 前缀，GLM 只查非空，Kimi 查 sk-kimi-），便于浏览器下调试错误提示
    const provider = mockDb.accounts.find((a) => a.id === accountId)?.provider;
    if (provider === "glm") {
      if (k === "") {
        throw new Error("API Key 格式不正确：不能为空");
      }
    } else {
      const isDeepSeek = provider === "deepseek";
      if (isDeepSeek ? !k.startsWith("sk-") : !k.startsWith("sk-kimi-")) {
        throw new Error(
          isDeepSeek ? "API Key 格式不正确：应以 sk- 开头" : "API Key 格式不正确：应以 sk-kimi- 开头",
        );
      }
      if (k.length < 12) {
        throw new Error("API Key 格式不正确：长度过短");
      }
    }
    mockDb.apiKeys[accountId] = k;
    return;
  }
  return invoke<void>("set_api_key", { accountId, key });
}

/** 清除该账号已保存的 API Key */
export async function clearApiKey(accountId: string): Promise<void> {
  if (!isTauri) {
    delete mockDb.apiKeys[accountId];
    return;
  }
  return invoke<void>("clear_api_key", { accountId });
}

/** 与后端 mask_api_key 同规则的脱敏：长度 > 12 显示 前 8 + "…" + 后 4，否则全显 */
function mockMaskApiKey(key: string): string {
  const chars = [...key];
  return chars.length > 12 ? `${chars.slice(0, 8).join("")}…${chars.slice(-4).join("")}` : key;
}

/** 每账号额外 API Key（本地消耗归属用）的登记上限（与后端一致） */
const MAX_EXTRA_API_KEYS = 5;

/**
 * 给该账号登记一把额外 API Key（同一账号在不同工具挂的多把 key 汇总归属到同一桶；
 * 只参与本地消耗归属，不参与任何网络请求）。
 * 后端按该账号 provider 复用主 Key 同款校验，重复/超上限抛中文错误原样透传
 */
export async function addAccountExtraKey(accountId: string, key: string): Promise<void> {
  if (!isTauri) {
    // mock 复刻后端语义：按 provider 校验 + 拒与主 Key/已有额外 Key 重复 + 上限 5
    const k = key.trim();
    const provider = mockDb.accounts.find((a) => a.id === accountId)?.provider;
    if (provider === "glm") {
      if (k === "") throw new Error("API Key 格式不正确：不能为空");
    } else {
      const isDeepSeek = provider === "deepseek";
      if (isDeepSeek ? !k.startsWith("sk-") : !k.startsWith("sk-kimi-")) {
        throw new Error(
          isDeepSeek ? "API Key 格式不正确：应以 sk- 开头" : "API Key 格式不正确：应以 sk-kimi- 开头",
        );
      }
    }
    if (mockDb.apiKeys[accountId] === k) {
      throw new Error("该 Key 已配置为主 API Key，无需重复登记");
    }
    const extras = mockDb.apiKeysExtra[accountId] ?? [];
    if (extras.includes(k)) throw new Error("该 Key 已在额外 Key 列表中");
    if (extras.length >= MAX_EXTRA_API_KEYS) {
      throw new Error(`每个账号最多登记 ${MAX_EXTRA_API_KEYS} 把额外 Key`);
    }
    mockDb.apiKeysExtra[accountId] = [...extras, k];
    return;
  }
  return invoke<void>("add_account_extra_key", { accountId, key });
}

/**
 * 移除该账号的一把额外 API Key：参数为脱敏串（UI 只持有脱敏串），
 * 后端对每把额外 key 重算脱敏后移除第一个精确匹配；无匹配抛中文错误原样透传
 */
export async function removeAccountExtraKey(accountId: string, masked: string): Promise<void> {
  if (!isTauri) {
    const extras = mockDb.apiKeysExtra[accountId] ?? [];
    const idx = extras.findIndex((k) => mockMaskApiKey(k) === masked.trim());
    if (idx < 0) throw new Error("未找到该额外 Key（可能已被移除）");
    mockDb.apiKeysExtra[accountId] = extras.filter((_, i) => i !== idx);
    return;
  }
  return invoke<void>("remove_account_extra_key", { accountId, masked });
}

/** 获取该账号的凭证配置状态（脱敏 Key + 各方式是否已配置） */
export async function getCredentialStatus(accountId: string): Promise<CredentialStatus> {
  if (!isTauri) return mockCredentialStatus(accountId);
  return invoke<CredentialStatus>("get_credential_status", { accountId });
}

// ============ 月度总量（网页 refresh_token）============

/**
 * 校验并保存该账号的网页凭证（月度总量用）。
 * 优先按新鉴权体系的 refresh_token 处理（后端自动续期）；旧体系 kimi-auth 值也兼容。
 * 后端先调接口校验，失败抛中文错误原样透传；成功返回当月月度总量。
 */
export async function setWebToken(accountId: string, token: string): Promise<MonthlyInfo> {
  if (!isTauri) {
    if (token.trim() === "") throw new Error("请粘贴 refresh_token 的值");
    // 模拟网络延迟，便于调试"校验中…"加载态
    await new Promise((resolve) => setTimeout(resolve, 600));
    mockDb.webTokens[accountId] = token.trim();
    return MOCK_MONTHLY;
  }
  return invoke<MonthlyInfo>("set_web_token", { accountId, token });
}

/** 清除该账号已保存的网页凭证（该页月度卡随之不再展示） */
export async function clearWebToken(accountId: string): Promise<void> {
  if (!isTauri) {
    delete mockDb.webTokens[accountId];
    return;
  }
  return invoke<void>("clear_web_token", { accountId });
}

/** 发起 OAuth 设备码登录（绑定指定账号）：返回 waiting 状态，后续进度由 device-login-updated 事件推送 */
export async function startDeviceLogin(accountId: string): Promise<DeviceLoginState> {
  if (!isTauri) {
    if (mockDeviceLoginTimer !== null) clearTimeout(mockDeviceLoginTimer);
    // 模拟用户 8 秒后完成浏览器授权
    mockDeviceLoginTimer = setTimeout(() => {
      mockDeviceLoginTimer = null;
      mockDb.oauthAccounts.add(accountId);
      const account = mockDb.accounts.find((a) => a.id === accountId);
      if (account) account.login_method = "oauth";
      emitMockDeviceLogin({ ...MOCK_IDLE_LOGIN, status: "success" });
    }, 8000);
    return mockWaitingLogin();
  }
  return invoke<DeviceLoginState>("start_device_login", { accountId });
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

/** 退出该账号的 OAuth 授权登录（清除本地 token） */
export async function oauthLogout(accountId: string): Promise<void> {
  if (!isTauri) {
    mockDb.oauthAccounts.delete(accountId);
    const account = mockDb.accounts.find((a) => a.id === accountId);
    if (account?.login_method === "oauth") account.login_method = null;
    return;
  }
  return invoke<void>("oauth_logout", { accountId });
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
