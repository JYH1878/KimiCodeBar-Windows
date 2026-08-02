import React, { useCallback, useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { useTranslation } from "react-i18next";
import "./styles.css";
import i18n, { resolveLang } from "./i18n";
import { useTheme } from "./theme";
import type { HistoryPoint, LocalUsageStats, PanelState, QuotaDetail, UpdateInfo } from "./types";
import { convertFileSrc } from "@tauri-apps/api/core";
import { checkUpdate, getLocalUsage, getPanelState, getSettings, getUsageHistory, isTauri, refreshNow, openSettings, openExternalUrl, onQuotaUpdated, onSettingsChanged, onUpdateInfo } from "./ipc";
import { UsageCard } from "./components/UsageCard";
import { MonthlyCard } from "./components/MonthlyCard";
import { TrendCard } from "./components/TrendCard";
import { LocalUsageCard } from "./components/LocalUsageCard";
import { MembershipCard } from "./components/MembershipCard";
import { BoosterCard } from "./components/BoosterCard";
import { ErrorBanner } from "./components/ErrorBanner";
import { EmptyState } from "./components/EmptyState";

/** epoch 秒 → 本地时间 HH:mm:ss */
function formatFetchedAt(epochSec: number): string {
  const d = new Date(epochSec * 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

/** 预设背景白名单（与后端 background.rs PRESETS / styles.css .bg-<id> 渐变一致；非法 id 按无背景处理） */
const BG_PRESETS = ["night", "aurora", "violet", "ember"];

/** 用量面板主界面（index.html 入口） */
function PanelApp() {
  const { t } = useTranslation();
  // 主题：读设置 + 跟随 settings-changed + system 模式跟随系统明暗
  useTheme();
  const [state, setState] = useState<PanelState | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  // 更新检查结果（仅 has_update 时有值，驱动底栏徽标）
  const [update, setUpdate] = useState<UpdateInfo | null>(null);
  // 历史采样点（趋势卡用）；null = 尚未加载
  const [history, setHistory] = useState<HistoryPoint[] | null>(null);
  // 本地 token 消耗统计（本地统计卡用）；null = 尚未加载
  const [localUsage, setLocalUsage] = useState<LocalUsageStats | null>(null);
  // 预设背景 id（纯 CSS 渐变 class）；null = 未选预设
  const [bgPreset, setBgPreset] = useState<string | null>(null);
  // 自定义背景图（kimibg:// 协议 URL）；null = 无图
  const [bgImage, setBgImage] = useState<string | null>(null);
  // 当前已加载背景（预设 + 文件名）：settings-changed 时按它判断要不要换（防每次保存都重拉图片）
  const bgRef = useRef<{ preset: string | null; image: string | null }>({ preset: null, image: null });
  // 每分钟触发一次重渲染，让重置倒计时保持新鲜
  const [, setTick] = useState(0);

  /** 按设置同步背景：预设（白名单校验，纯 CSS class）+ 自定义图（协议 URL，加版本 query 强制重拉） */
  const syncBackground = useCallback((preset: string | null | undefined, filename: string | null | undefined) => {
    const p = preset && BG_PRESETS.includes(preset) ? preset : null;
    const name = filename ?? null;
    if (p === bgRef.current.preset && name === bgRef.current.image) return;
    bgRef.current = { preset: p, image: name };
    setBgPreset(p);
    if (name === null || !isTauri) {
      setBgImage(null);
      return;
    }
    // 同格式换图文件名不变，靠版本 query 让 webview 重新拉取（协议侧已 no-store）
    setBgImage(`${convertFileSrc("bg", "kimibg")}?v=${Date.now()}`);
  }, []);

  // 语言：挂载时读设置应用一次，之后跟随 settings-changed 广播即时切换
  useEffect(() => {
    getSettings()
      .then((s) => {
        void i18n.changeLanguage(resolveLang(s.language));
        syncBackground(s.background_preset, s.background_image);
      })
      .catch(() => {
        // 设置读取失败保持系统语言，不影响面板功能
      });
    return onSettingsChanged((s) => {
      void i18n.changeLanguage(resolveLang(s.language));
      syncBackground(s.background_preset, s.background_image);
    });
  }, [syncBackground]);

  // 手动刷新：成功用返回值整体替换；失败把错误写进横幅，保留已有缓存
  const doRefresh = useCallback(async () => {
    setRefreshing(true);
    try {
      setState(await refreshNow());
    } catch (e) {
      setState((prev) => (prev ? { ...prev, error: String(e) } : prev));
    } finally {
      setRefreshing(false);
    }
  }, []);

  useEffect(() => {
    let alive = true;
    // 先取缓存状态立即渲染，保证断网也能秒开
    getPanelState()
      .then((s) => {
        if (!alive) return;
        setState(s);
        // 已配置凭证则紧接着后台刷新一次最新数据
        if (s.credential) void doRefresh();
      })
      .catch((e) => {
        if (!alive) return;
        // get_panel_state 本身失败（理论上不应发生）：按无凭证 + 错误兜底
        setState({
          credential: false,
          loading: false,
          quota: null,
          fetched_at: null,
          error: String(e),
          low_warning: false,
        });
      });
    // 订阅后端主动推送（定时刷新完成后广播的 quota-updated）
    const unlisten = onQuotaUpdated((s) => {
      setState(s);
      // 每次刷新成功后历史采样会增长，同步重拉趋势（失败静默，保留旧曲线）
      getUsageHistory()
        .then((h) => setHistory(h))
        .catch(() => {});
      // 本地 token 统计也随之可能有新扫描结果，同步重拉（失败静默）
      getLocalUsage()
        .then((u) => setLocalUsage(u))
        .catch(() => {});
    });
    // 与首屏状态并行拉一次历史采样（趋势卡用）；失败静默，卡片显示"数据积累中…"
    getUsageHistory()
      .then((h) => {
        if (alive) setHistory(h);
      })
      .catch(() => {});
    // 与首屏状态并行拉一次本地 token 统计（失败静默，卡片不渲染）
    getLocalUsage()
      .then((u) => {
        if (alive) setLocalUsage(u);
      })
      .catch(() => {});
    // 与首屏状态并行检查一次更新；失败（含 error 字段）静默，不打扰用户
    checkUpdate()
      .then((info) => {
        if (alive && info.has_update) setUpdate(info);
      })
      .catch(() => {});
    // 订阅后端主动推送的更新检查结果（托盘打开面板时的后台检查完成会广播 update-info）
    const unlistenUpdate = onUpdateInfo((info) => {
      if (info.has_update) setUpdate(info);
    });
    const timer = setInterval(() => setTick((t) => t + 1), 60_000);
    return () => {
      alive = false;
      unlisten();
      unlistenUpdate();
      clearInterval(timer);
    };
  }, [doRefresh]);

  // 背景：预设为纯 CSS 渐变 class（底色干净，不压遮罩）；自定义图压固定浓度遮罩。预设优先于图片
  const bgStyle =
    bgPreset === null && bgImage !== null
      ? { backgroundImage: `linear-gradient(var(--bg-scrim), var(--bg-scrim)), url("${bgImage}")` }
      : undefined;
  const panelCls = `panel${bgPreset !== null || bgImage !== null ? " has-bg" : ""}${bgPreset !== null ? ` bg-${bgPreset}` : ""}`;

  // 首屏：状态未返回，或后端加载中且没有缓存/错误可展示 → 居中加载动画
  if (state === null || (state.quota === null && state.error === null && state.loading)) {
    return (
      <div className={`${panelCls} loading-center`} style={bgStyle}>
        <div className="spinner" />
        <p className="muted-text">{t("panel.loading")}</p>
      </div>
    );
  }

  // 未配置凭证：只显示引导
  if (!state.credential) {
    return (
      <div className={panelCls} style={bgStyle}>
        <EmptyState onOpenSettings={() => void openSettings()} />
      </div>
    );
  }

  const quota = state.quota;
  // 总额卡复用 UsageCard：拼一个无重置时间的 QuotaDetail（used 由 limit-remaining 推算）
  const totalDetail: QuotaDetail | null = quota?.total
    ? {
        used: quota.total.limit - quota.total.remaining,
        limit: quota.total.limit,
        remaining: quota.total.remaining,
        percent_remaining: quota.total.percent_remaining,
      }
    : null;

  const busy = refreshing || state.loading;
  // 有新版本且拿到发布页地址时，底栏"更新于"左侧显示更新徽标
  const updateUrl = update?.has_update ? update.release_url : null;

  return (
    <div className={panelCls} style={bgStyle}>
      {state.error !== null && (
        <ErrorBanner error={state.error} onRetry={() => void doRefresh()} />
      )}
      {quota?.weekly && <UsageCard title={t("panel.weeklyUsage")} detail={quota.weekly} />}
      {quota?.five_hour && <UsageCard title={t("panel.fiveHourUsage")} detail={quota.five_hour} />}
      {/* 月度总量（网页 token）：monthly 与 monthly_error 都为空时整卡不渲染 */}
      {state.monthly && <MonthlyCard monthly={state.monthly} />}
      {state.monthly_error && <p className="monthly-error">{state.monthly_error}</p>}
      {/* 用量趋势（本地历史采样，纯事实不预测）：月度总量卡之后、本地统计卡之前 */}
      <TrendCard points={history} />
      {/* 本地 Token 消耗（扫描 wire.jsonl）：趋势卡之后、会员/Booster 行之前；
          未扫描过（last_scan_at 为空）时整卡不渲染 */}
      <LocalUsageCard stats={localUsage} />
      {totalDetail && <UsageCard title={t("panel.totalQuota")} detail={totalDetail} />}
      {quota && (
        <div className="mini-row">
          <MembershipCard level={quota.membership_level} />
          <BoosterCard booster={quota.booster} />
        </div>
      )}
      <div className="footer">
        <span className="fetched-at">
          {updateUrl !== null && update?.latest && (
            <button
              type="button"
              className="update-badge"
              onClick={() => void openExternalUrl(updateUrl)}
              title={t("panel.updateBadgeTitle")}
            >
              ⬆ v{update.latest}
            </button>
          )}
          {state.fetched_at
            ? t("panel.updatedAt", { time: formatFetchedAt(state.fetched_at) })
            : t("panel.noData")}
        </span>
        <div className="footer-actions">
          <button
            className="btn"
            onClick={() => void doRefresh()}
            disabled={busy}
            title={t("panel.refreshTitle")}
          >
            <span className={busy ? "spin" : ""}>⟳</span> {t("panel.refresh")}
          </button>
          <button className="btn" onClick={() => void openSettings()} title={t("panel.settingsTitle")}>
            {t("panel.settings")}
          </button>
        </div>
      </div>
    </div>
  );
}

createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <PanelApp />
  </React.StrictMode>,
);
