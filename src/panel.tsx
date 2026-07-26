import React, { useCallback, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import "./styles.css";
import type { HistoryPoint, PanelState, QuotaDetail, UpdateInfo } from "./types";
import { checkUpdate, getPanelState, getUsageHistory, refreshNow, openSettings, openExternalUrl, onQuotaUpdated, onUpdateInfo } from "./ipc";
import { UsageCard } from "./components/UsageCard";
import { MonthlyCard } from "./components/MonthlyCard";
import { TrendCard } from "./components/TrendCard";
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

/** 用量面板主界面（index.html 入口） */
function PanelApp() {
  const [state, setState] = useState<PanelState | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  // 更新检查结果（仅 has_update 时有值，驱动底栏徽标）
  const [update, setUpdate] = useState<UpdateInfo | null>(null);
  // 历史采样点（趋势卡用）；null = 尚未加载
  const [history, setHistory] = useState<HistoryPoint[] | null>(null);
  // 每分钟触发一次重渲染，让重置倒计时保持新鲜
  const [, setTick] = useState(0);

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
    });
    // 与首屏状态并行拉一次历史采样（趋势卡用）；失败静默，卡片显示"数据积累中…"
    getUsageHistory()
      .then((h) => {
        if (alive) setHistory(h);
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

  // 首屏：状态未返回，或后端加载中且没有缓存/错误可展示 → 居中加载动画
  if (state === null || (state.quota === null && state.error === null && state.loading)) {
    return (
      <div className="panel loading-center">
        <div className="spinner" />
        <p className="muted-text">加载中…</p>
      </div>
    );
  }

  // 未配置凭证：只显示引导
  if (!state.credential) {
    return (
      <div className="panel">
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
    <div className="panel">
      {state.error !== null && (
        <ErrorBanner error={state.error} onRetry={() => void doRefresh()} />
      )}
      {quota?.weekly && <UsageCard title="7 天用量" detail={quota.weekly} />}
      {quota?.five_hour && <UsageCard title="5 小时用量" detail={quota.five_hour} />}
      {/* 月度总量（网页 token 可选增强）：monthly 与 monthly_error 都为空时整卡不渲染 */}
      {state.monthly && <MonthlyCard monthly={state.monthly} />}
      {state.monthly_error && <p className="monthly-error">{state.monthly_error}</p>}
      {/* 用量趋势（本地历史采样，纯事实不预测）：月度总量卡之后、会员/Booster 行之前 */}
      <TrendCard points={history} />
      {totalDetail && <UsageCard title="总额度" detail={totalDetail} />}
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
              title="发现新版本，点击打开发布页"
            >
              ⬆ v{update.latest}
            </button>
          )}
          {state.fetched_at ? `更新于 ${formatFetchedAt(state.fetched_at)}` : "暂无数据"}
        </span>
        <div className="footer-actions">
          <button
            className="btn"
            onClick={() => void doRefresh()}
            disabled={busy}
            title="立即刷新"
          >
            <span className={busy ? "spin" : ""}>⟳</span> 刷新
          </button>
          <button className="btn" onClick={() => void openSettings()} title="打开设置">
            设置
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
