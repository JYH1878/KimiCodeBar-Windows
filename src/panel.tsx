import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { useTranslation } from "react-i18next";
import "./styles.css";
import i18n, { resolveLang } from "./i18n";
import { useTheme } from "./theme";
import type { AccountPanel, HistoryPoint, LocalUsageStats, PanelState, QuotaDetail, UpdateInfo } from "./types";
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
import { Tuanzi } from "./components/Tuanzi";

/** epoch 秒 → 本地时间 HH:mm:ss */
function formatFetchedAt(epochSec: number): string {
  const d = new Date(epochSec * 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

/** 预设背景白名单（与后端 background.rs PRESETS / styles.css .bg-<id> 渐变一致；非法 id 按无背景处理） */
const BG_PRESETS = ["night", "aurora", "violet", "ember"];

/** 翻页动画时长（与 styles.css .pager-track 的 transition 一致）：动画期间忽略滚轮连击 */
const PAGE_ANIM_MS = 380;
/** 滚轮/拖拽触发翻页的阈值（像素） */
const PAGE_FLIP_THRESHOLD = 50;

/** 单账号页：页头账号名 + 该账号的卡片组（复用现有卡片，数据全部按账号取） */
function AccountPage({
  panel,
  history,
  localUsage,
  onRetry,
  onOpenSettings,
}: {
  panel: AccountPanel;
  /** 该账号的历史采样点；null = 尚未加载 */
  history: HistoryPoint[] | null;
  /** 本地 token 统计（机器级数据，各页同一份） */
  localUsage: LocalUsageStats | null;
  onRetry: () => void;
  onOpenSettings: () => void;
}) {
  const { t } = useTranslation();
  const quota = panel.quota;

  return (
    <section className="page">
      <header className="page-head">
        <span className="page-title">{panel.account.name}</span>
      </header>
      {/* 该账号未配置任何凭证：页内引导（不挡其他账号页） */}
      {!panel.credential ? (
        <EmptyState onOpenSettings={onOpenSettings} />
      ) : quota === null && panel.error === null ? (
        /* 有凭证但还没有任何数据（首刷进行中）：页内加载动画 */
        <div className="page-loading">
          <div className="spinner" />
          <p className="muted-text">{t("panel.loading")}</p>
        </div>
      ) : (
        <>
          {panel.error !== null && <ErrorBanner error={panel.error} onRetry={onRetry} />}
          {quota?.weekly && <UsageCard title={t("panel.weeklyUsage")} detail={quota.weekly} />}
          {quota?.five_hour && <UsageCard title={t("panel.fiveHourUsage")} detail={quota.five_hour} />}
          {/* 月度总量（网页 token）：monthly 与 monthly_error 都为空时整卡不渲染 */}
          {panel.monthly && <MonthlyCard monthly={panel.monthly} />}
          {panel.monthly_error && <p className="monthly-error">{panel.monthly_error}</p>}
          {/* 用量趋势（该账号的本地历史采样，纯事实不预测） */}
          <TrendCard points={history} />
          {/* 本地 Token 消耗（扫描 wire.jsonl）：机器级数据，每页显示同一份；
              未扫描过（last_scan_at 为空）时整卡不渲染 */}
          <LocalUsageCard stats={localUsage} />
          {quota?.total && (
            <UsageCard
              title={t("panel.totalQuota")}
              // 总额卡复用 UsageCard：拼一个无重置时间的 QuotaDetail（used 由 limit-remaining 推算）
              detail={{
                used: quota.total.limit - quota.total.remaining,
                limit: quota.total.limit,
                remaining: quota.total.remaining,
                percent_remaining: quota.total.percent_remaining,
              } satisfies QuotaDetail}
            />
          )}
          {quota && (
            <div className="mini-row">
              <MembershipCard level={quota.membership_level} />
              <BoosterCard booster={quota.booster} />
            </div>
          )}
        </>
      )}
    </section>
  );
}

/** 用量面板主界面（index.html 入口）：横向翻页容器，一页一个账号，像手机桌面 */
function PanelApp() {
  const { t } = useTranslation();
  // 主题：读设置 + 跟随 settings-changed + system 模式跟随系统明暗
  useTheme();
  const [state, setState] = useState<PanelState | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  // 更新检查结果（仅 has_update 时有值，驱动底栏徽标）
  const [update, setUpdate] = useState<UpdateInfo | null>(null);
  // 各账号的历史采样点（趋势卡用，按账号 id 索引）；未加载的账号缺 key
  const [historyMap, setHistoryMap] = useState<Record<string, HistoryPoint[]>>({});
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

  // ---- 翻页状态：当前页下标（到头停不循环） ----
  const [page, setPage] = useState(0);
  // 拖拽中的横向位移（px）；非拖拽态为 null（此时轨道带滑动动画）
  const [dragDelta, setDragDelta] = useState<number | null>(null);
  // 拖拽跟踪：超过 8px 才认定是拖拽并捕获指针（否则是点击，放行给页内按钮）
  const dragRef = useRef<{ startX: number; delta: number; capturing: boolean } | null>(null);
  // 动画期间锁滚轮，防连击翻过多个
  const wheelLockRef = useRef(false);
  // page 的 ref 镜像（事件回调里读最新页码，避免闭包过期）
  const pageRef = useRef(0);
  pageRef.current = page;

  const accounts = useMemo(() => state?.accounts ?? [], [state]);
  const pageCount = accounts.length;
  // pageCount 的 ref 镜像（goTo 不依赖 pageCount，避免回调身份变化）
  const pageCountRef = useRef(0);
  pageCountRef.current = pageCount;

  /** 翻到指定页（越界钳制：到头停不循环） */
  const goTo = useCallback((target: number) => {
    setPage(Math.max(0, Math.min(target, Math.max(0, pageCountRef.current - 1))));
  }, []);

  // 账号增删后页码可能越界（如删掉最后一页）：钳回范围内
  useEffect(() => {
    if (page > pageCount - 1) setPage(Math.max(0, pageCount - 1));
  }, [page, pageCount]);

  // 滚轮翻页：纵向/横向滚动都算，越过阈值翻一页；动画期间忽略
  const onWheel = useCallback(
    (e: React.WheelEvent) => {
      if (pageCount <= 1) return;
      const d = Math.abs(e.deltaX) > Math.abs(e.deltaY) ? e.deltaX : e.deltaY;
      if (wheelLockRef.current || Math.abs(d) < 20) return;
      const dir = d > 0 ? 1 : -1;
      const next = pageRef.current + dir;
      if (next < 0 || next >= pageCountRef.current) return;
      wheelLockRef.current = true;
      setTimeout(() => {
        wheelLockRef.current = false;
      }, PAGE_ANIM_MS);
      goTo(next);
    },
    [goTo, pageCount],
  );

  // 鼠标拖拽翻页：移动超 8px 才跟手（之前不捕获指针，页内按钮点击不受影响），松手过阈值翻页/回弹
  const onPointerDown = useCallback((e: React.PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    dragRef.current = { startX: e.clientX, delta: 0, capturing: false };
  }, []);
  const onPointerMove = useCallback((e: React.PointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (drag === null) return;
    const delta = e.clientX - drag.startX;
    if (!drag.capturing) {
      if (Math.abs(delta) < 8) return;
      drag.capturing = true;
      e.currentTarget.setPointerCapture(e.pointerId);
    }
    drag.delta = delta;
    setDragDelta(delta);
  }, []);
  const endDrag = useCallback(() => {
    const drag = dragRef.current;
    dragRef.current = null;
    setDragDelta(null);
    if (drag === null || !drag.capturing || pageCountRef.current <= 1) return;
    if (Math.abs(drag.delta) >= PAGE_FLIP_THRESHOLD) {
      goTo(pageRef.current + (drag.delta < 0 ? 1 : -1));
    }
  }, [goTo]);

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

  /** 拉取某账号的历史采样（失败静默，保留旧曲线） */
  const fetchHistory = useCallback((accountId: string) => {
    getUsageHistory(accountId)
      .then((h) => setHistoryMap((m) => ({ ...m, [accountId]: h })))
      .catch(() => {});
  }, []);

  // 手动刷新：成功用返回值整体替换；失败把错误写进当前页横幅，保留已有缓存
  const doRefresh = useCallback(async () => {
    setRefreshing(true);
    try {
      setState(await refreshNow());
    } catch (e) {
      setState((prev) => {
        if (prev === null) return prev;
        const cur = prev.accounts[pageRef.current];
        if (cur === undefined) return prev;
        const accounts = [...prev.accounts];
        accounts[pageRef.current] = { ...cur, error: String(e) };
        return { ...prev, accounts };
      });
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
        // 有任一账号已配置凭证则紧接着后台刷新一次最新数据
        if (s.accounts.some((a) => a.credential)) void doRefresh();
      })
      .catch((e) => {
        if (!alive) return;
        // get_panel_state 本身失败（理论上不应发生）：按无账号 + 空态兜底
        console.error("get_panel_state failed:", e);
        setState({ loading: false, accounts: [] });
      });
    // 订阅后端主动推送（定时刷新完成后广播的 quota-updated）
    const unlisten = onQuotaUpdated((s) => {
      setState(s);
      // 每次刷新成功后历史采样会增长，同步重拉当前页账号的趋势（失败静默，保留旧曲线）
      const cur = s.accounts[pageRef.current];
      if (cur !== undefined) fetchHistory(cur.account.id);
      // 本地 token 统计也随之可能有新扫描结果，同步重拉（失败静默）
      getLocalUsage()
        .then((u) => setLocalUsage(u))
        .catch(() => {});
    });
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
  }, [doRefresh, fetchHistory]);

  // 翻页后按需补拉该账号的历史采样（还没拉过的话）
  useEffect(() => {
    const cur = accounts[page];
    if (cur !== undefined && historyMap[cur.account.id] === undefined) {
      fetchHistory(cur.account.id);
    }
  }, [page, accounts, historyMap, fetchHistory]);

  // 背景：预设为纯 CSS 渐变 class（底色干净，不压遮罩）；自定义图压固定浓度遮罩。预设优先于图片
  const bgStyle =
    bgPreset === null && bgImage !== null
      ? { backgroundImage: `linear-gradient(var(--bg-scrim), var(--bg-scrim)), url("${bgImage}")` }
      : undefined;
  const panelCls = `panel${bgPreset !== null || bgImage !== null ? " has-bg" : ""}${bgPreset !== null ? ` bg-${bgPreset}` : ""}`;

  // 首屏：状态未返回 → 居中加载动画
  if (state === null) {
    return (
      <div className={`${panelCls} loading-center`} style={bgStyle}>
        <div className="spinner" />
        <p className="muted-text">{t("panel.loading")}</p>
      </div>
    );
  }

  // 一个账号都没有（首装未配置 / 全删了）：整页引导去设置页添加账号
  if (accounts.length === 0) {
    return (
      <div className={panelCls} style={bgStyle}>
        <EmptyState onOpenSettings={() => void openSettings("account-add")} />
      </div>
    );
  }

  const current = accounts[Math.min(page, accounts.length - 1)];
  const busy = refreshing || state.loading;
  // 有新版本且拿到发布页地址时，底栏"更新于"左侧显示更新徽标
  const updateUrl = update?.has_update ? update.release_url : null;

  // 轨道位移：拖拽态跟手（无动画），非拖拽态按页码定位（带滑动动画）
  const trackStyle: React.CSSProperties = {
    transform: `translateX(calc(${-page * 100}% + ${dragDelta ?? 0}px))`,
    transition: dragDelta === null ? undefined : "none",
  };

  return (
    <div className={panelCls} style={bgStyle}>
      {/* 横向翻页容器：滚轮 / 拖拽 / 圆点三种翻页方式，到头停不循环 */}
      <div
        className="pager"
        onWheel={onWheel}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={endDrag}
        onPointerCancel={endDrag}
      >
        <div className="pager-track" style={trackStyle}>
          {accounts.map((a) => (
            <AccountPage
              key={a.account.id}
              panel={a}
              history={historyMap[a.account.id] ?? null}
              localUsage={localUsage}
              onRetry={() => void doRefresh()}
              onOpenSettings={() => void openSettings()}
            />
          ))}
        </div>
      </div>

      {/* 圆点导航：N 个账号圆点 + 末尾「+」（点开设置页并定位到账号添加表单） */}
      <nav className="pager-dots" aria-label={t("panel.pagesAria")}>
        {accounts.map((a, i) => (
          <button
            key={a.account.id}
            type="button"
            className={`dot${i === page ? " active" : ""}${a.low_warning ? " low" : ""}`}
            title={a.account.name}
            aria-label={a.account.name}
            onClick={() => goTo(i)}
          />
        ))}
        <button
          type="button"
          className="dot dot-add"
          title={t("panel.addAccountTitle")}
          aria-label={t("panel.addAccountTitle")}
          onClick={() => void openSettings("account-add")}
        >
          +
        </button>
      </nav>

      <div className="footer">
        {/* 底栏团子（矢量 SVG，复刻上游 AnimatedKimiCodeLogo，纯装饰） */}
        <Tuanzi className="mascot" />
        <div className="footer-right">
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
            {current.fetched_at
              ? t("panel.updatedAt", { time: formatFetchedAt(current.fetched_at) })
              : t("panel.noData")}
          </span>
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
