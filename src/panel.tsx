import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import "./styles.css";
import i18n, { resolveLang } from "./i18n";
import { useTheme } from "./theme";
import type { AccountPanel, HistoryPoint, LocalUsageStats, PanelState, QuotaDetail, UpdateInfo } from "./types";
import { convertFileSrc } from "@tauri-apps/api/core";
import { checkUpdate, getLocalUsage, getPanelState, getSettings, getUsageHistory, isTauri, refreshNow, openSettings, openExternalUrl, onQuotaUpdated, onSettingsChanged, onUpdateInfo, setPanelContentHeight } from "./ipc";
import { UsageCard, resetTimeText } from "./components/UsageCard";
import { HeroPair } from "./components/HeroPair";
import { MonthlyCard } from "./components/MonthlyCard";
import { TrendCard } from "./components/TrendCard";
import { LocalUsageCard, formatTokens } from "./components/LocalUsageCard";
import { MembershipCard } from "./components/MembershipCard";
import { BoosterCard } from "./components/BoosterCard";
import { ErrorBanner } from "./components/ErrorBanner";
import { EmptyState } from "./components/EmptyState";
import { DeepSeekBalanceCard, formatMoney } from "./components/DeepSeekBalanceCard";
import { Tuanzi } from "./components/Tuanzi";

/** epoch 秒 → 本地时间 HH:mm:ss */
function formatFetchedAt(epochSec: number): string {
  const d = new Date(epochSec * 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

/** 预设背景白名单（与后端 background.rs PRESETS / styles.css .bg-<id> 渐变一致；非法 id 按无背景处理） */
const BG_PRESETS = ["night", "aurora", "violet", "ember"];

/** 翻页滑动的名义时长：弹簧时代翻页无固定时长，此值仅用于「压矮窗口延迟到滑动结束」 */
const PAGE_ANIM_MS = 380;
/** 拖拽 commit 阈值（px）：越过才捕获指针并 1:1 跟手，之前放行页内点击 */
const DRAG_COMMIT_PX = 10;
/** 滚轮翻页累积阈值（delta 像素，200ms 静默后重新累积）：触摸板轻微滚动不翻页 */
const WHEEL_FLIP_ACCUM = 50;
/** 弹簧响应（秒，越小越利落）：非"时长"——沉降时间由参数涌现 */
const SPRING_RESPONSE = 0.35;
/** 阻尼比：1.0 = 临界阻尼（默认不回弹）；0.8 = 甩动释放专用的轻微回弹 */
const SPRING_DAMPING_DEFAULT = 1.0;
const SPRING_DAMPING_FLICK = 0.8;
/** 判定「甩动」的释放速度下限（px/s）：超过才允许回弹 */
const FLICK_VELOCITY = 250;

/** §6 动量投影：指数衰减模型预测松手后的滑行距离（px），手感同 iOS 滚动 */
function project(velocity: number, decelerationRate = 0.998): number {
  return (velocity / 1000) * (decelerationRate / (1 - decelerationRate));
}

/** §9 橡皮筋边界：越过边界越多跟随越少（渐进阻尼，不硬停） */
function rubberband(overshoot: number, dimension: number, constant = 0.55): number {
  return (overshoot * dimension * constant) / (dimension + constant * Math.abs(overshoot));
}

/** 跟随系统「减少动态效果」（Windows 辅助功能关动画时命中）：翻页降级为 200ms 淡入淡出 */
function usePrefersReducedMotion(): boolean {
  const [reduced, setReduced] = useState(() => window.matchMedia("(prefers-reduced-motion: reduce)").matches);
  useEffect(() => {
    const mq = window.matchMedia("(prefers-reduced-motion: reduce)");
    const onChange = () => setReduced(mq.matches);
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);
  return reduced;
}

/** 面板 chrome 高度：上下 padding 18 + 两条 flex gap 20 - 底栏 margin-top 2（styles.css .panel/.footer） */
const PANEL_CHROME_PX = 36;

/** 内容高度自适应：观测当前页的 .page-body，内容/页码变化时把目标窗口高（内容+圆点+底栏+chrome）
 *  推给后端；后端据此重算窗口尺寸并重定位（隐藏时记忆，show 时校准）。 */
function usePanelContentHeight(page: number, ready: boolean) {
  useEffect(() => {
    if (!ready || !isTauri) return;
    const track = document.querySelector(".pager-track");
    const pageBody = track?.children[page]?.querySelector(".page-body");
    const dots = document.querySelector(".pager-dots");
    const footer = document.querySelector(".footer");
    if (!(pageBody instanceof HTMLElement) || !(dots instanceof HTMLElement) || !(footer instanceof HTMLElement)) {
      return;
    }
    let raf = 0;
    let shrinkTimer: ReturnType<typeof setTimeout> | null = null;
    const measure = () => {
      // 用 getBoundingClientRect 取小数值、向上取整后 +5 逻辑像素兜底：
      // 逻辑高 × DPI 缩放比存在半像素舍入竞争（614.3 × 1.75 = 1075.5 物理像素，向下舍入
      // 会让 .page 冒出亚像素溢出滚动条），+5 逻辑像素（肉眼不可见）保证恒不溢出
      const contentH = pageBody.getBoundingClientRect().height;
      const desired =
        Math.ceil(
          contentH + dots.getBoundingClientRect().height + footer.getBoundingClientRect().height + PANEL_CHROME_PX,
        ) + 5;
      const cur = window.innerHeight;
      if (desired > cur + 0.5) {
        // 长高立即发：延迟会让高页内容在旧矮窗口里溢出；
        // 减少动态模式下后端瞬时到位（不播高度缓动）
        setPanelContentHeight(desired, !window.matchMedia("(prefers-reduced-motion: reduce)").matches);
      } else if (desired < cur - 2) {
        // 压矮延迟到翻页滑动结束后发：动画途中压矮会让正在滑出的高页瞬间溢出
        // （滚动条一闪而过 + 内容被裁切）；滑动期间滚动条另由 .sliding 屏蔽
        if (shrinkTimer !== null) clearTimeout(shrinkTimer);
        const animate = !window.matchMedia("(prefers-reduced-motion: reduce)").matches;
        shrinkTimer = setTimeout(() => setPanelContentHeight(desired, animate), PAGE_ANIM_MS);
      }
    };
    // rAF 合并同一帧内的多次布局变化；后端另有 2px 阈值防抖
    const ro = new ResizeObserver(() => {
      cancelAnimationFrame(raf);
      raf = requestAnimationFrame(measure);
    });
    ro.observe(pageBody);
    return () => {
      cancelAnimationFrame(raf);
      if (shrinkTimer !== null) clearTimeout(shrinkTimer);
      ro.disconnect();
    };
  }, [page, ready]);
}

/** 单账号页：页头账号名 + 该账号的卡片组（复用现有卡片，数据全部按账号取）。
 *  极简模式只保留页头 / 错误横幅 / 7天·5小时额度条（未配置凭证引导照常） */
function AccountPage({
  panel,
  history,
  localUsage,
  minimal,
  onRetry,
  onOpenSettings,
}: {
  panel: AccountPanel;
  /** 该账号的历史采样点；null = 尚未加载 */
  history: HistoryPoint[] | null;
  /** 该账号的本地 token 统计（按 CLI 凭证归属）；null = 尚未加载 */
  localUsage: LocalUsageStats | null;
  /** 极简模式：隐藏月度/趋势/本地统计/会员/Booster 等卡片 */
  minimal: boolean;
  onRetry: () => void;
  onOpenSettings: () => void;
}) {
  const { t } = useTranslation();
  const quota = panel.quota;
  const isDeepSeek = panel.account.provider === "deepseek";
  // GLM 页复用 Kimi 额度卡（额度本就映射进 KimiQuota 契约），跳过月度/总额/Booster 卡（本地统计卡照常显示）
  const isGlm = panel.account.provider === "glm";
  const balance = panel.deepseek_balance ?? null;

  return (
    <section className="page">
      {/* page-body：内容流容器（fit-content 供窗口自适应测高） */}
      <div className="page-body">
      <header className="page-head">
        <span className="page-title">{panel.account.name}</span>
        {/* 提供商徽章：Kimi / DeepSeek / GLM 分色胶囊，样式由 .page-head flex 与名称胶囊居中对齐 */}
        <span className={`badge provider-badge pb-${panel.account.provider}`}>{isDeepSeek ? "DeepSeek" : isGlm ? "GLM" : "Kimi"}</span>
      </header>
      {/* 该账号未配置任何凭证：页内引导（不挡其他账号页） */}
      {!panel.credential ? (
        <EmptyState onOpenSettings={onOpenSettings} />
      ) : isDeepSeek ? (
        /* DeepSeek 页：余额卡 + 不可用横幅 + 错误横幅（极简模式照旧这一张卡） */
        balance === null && panel.error === null ? (
          /* 有凭证但还没有任何数据（首刷进行中）：页内加载动画 */
          <div className="page-loading">
            <div className="spinner" />
            <p className="muted-text">{t("panel.loading")}</p>
          </div>
        ) : (
          <>
            {panel.error !== null && <ErrorBanner error={panel.error} onRetry={onRetry} />}
            {balance !== null && !balance.is_available && (
              <div className="error-banner">
                <span className="error-text">{t("deepseek.unavailableBanner")}</span>
              </div>
            )}
            {balance !== null && (
              <DeepSeekBalanceCard balance={balance} fetchedAt={panel.fetched_at} low={panel.low_warning} />
            )}
            {/* 本地 Token 消耗按账号归属，DeepSeek 页同样显示（极简模式隐藏） */}
            {!minimal && <LocalUsageCard stats={localUsage} />}
          </>
        )
      ) : quota === null && panel.error === null ? (
        /* 有凭证但还没有任何数据（首刷进行中）：页内加载动画 */
        <div className="page-loading">
          <div className="spinner" />
          <p className="muted-text">{t("panel.loading")}</p>
        </div>
      ) : (
        <>
          {panel.error !== null && <ErrorBanner error={panel.error} onRetry={onRetry} />}
          {/* Hero 双联卡：7 天 / 5 小时并置（极简模式下同样只保留这一组） */}
          <HeroPair weekly={quota?.weekly} fiveHour={quota?.five_hour} />
          {/* 以下卡片极简模式全部隐藏：月度总量 / 趋势 / 本地统计 / 总额 / 会员 / Booster。
              GLM 页只有趋势 / 本地统计 / 会员档位（无月度/总额/Booster 接口） */}
          {!minimal && (
            <>
              {/* 月度总量（网页 token，仅 Kimi 账号）：monthly 与 monthly_error 都为空时整卡不渲染 */}
              {!isGlm && panel.monthly && <MonthlyCard monthly={panel.monthly} />}
              {!isGlm && panel.monthly_error && <p className="monthly-error">{panel.monthly_error}</p>}
              {/* 用量趋势（该账号的本地历史采样，纯事实不预测；GLM 同样写采样） */}
              <TrendCard points={history} />
              {/* 本地 Token 消耗（扫描 wire.jsonl 按 CLI 凭证归属）：各 provider 账号同显；
                  未扫描过（last_scan_at 为空）时整卡不渲染 */}
              <LocalUsageCard stats={localUsage} />
              {!isGlm && quota?.total && (
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
                  {!isGlm && <BoosterCard booster={quota.booster} />}
                </div>
              )}
            </>
          )}
        </>
      )}
      </div>
    </section>
  );
}

/** 总览状态灯三态：绿=有数据且未越阈；红=后端 low_warning（DeepSeek 的 is_available=false
 *  后端已并入，前端不重算阈值；拉取失败恒不为红）；灰=未配置/加载中/无缓存失败 */
type OvDotState = "ok" | "low" | "none";

function ovDotState(panel: AccountPanel): OvDotState {
  if (!panel.credential) return "none";
  const hasData =
    panel.account.provider === "deepseek" ? (panel.deepseek_balance ?? null) !== null : panel.quota !== null;
  if (!hasData) return "none";
  return panel.low_warning ? "low" : "ok";
}

/** 消耗视图行体：今日大号等宽 token 数 + 昨日小字；未拉取显示占位、从未扫描显示暂无数据 */
function ovBurnBody(t: TFunction, stats: LocalUsageStats | null): React.ReactNode {
  if (stats === null) return <p className="ov-hint">…</p>;
  if (stats.last_scan_at === null) return <p className="ov-hint">{t("panel.noData")}</p>;
  return (
    <div className="ov-burn">
      <span className="ov-burn-today">{formatTokens(stats.today_tokens)}</span>
      <span className="ov-burn-label">{t("localUsage.today")}</span>
      <span className="ov-burn-yesterday">
        {t("localUsage.yesterday", { tokens: formatTokens(stats.yesterday_tokens) })}
      </span>
    </div>
  );
}

/** 总额度监控行（正常模式）：状态灯 + 名称 + 提供商分色徽章 +
 *  额度视图（Kimi/GLM 双窗口「标签+重置+剩%」行 + 迷你条；DeepSeek 余额）/ 消耗视图（今日 token）。
 *  点击跳该账号详情页。 */
function OverviewRow({
  panel,
  seg,
  stats,
  onOpen,
}: {
  panel: AccountPanel;
  seg: "quota" | "burn";
  /** 该账号的本地 token 统计（消耗视图用）；null = 尚未加载 */
  stats: LocalUsageStats | null;
  onOpen: () => void;
}) {
  const { t } = useTranslation();
  const isDeepSeek = panel.account.provider === "deepseek";
  const quota = panel.quota;
  const balance = panel.deepseek_balance ?? null;
  // 有无缓存数据（quota / deepseek_balance），决定「加载中 / 失败 / 数据」三分支
  const hasData = isDeepSeek ? balance !== null : quota !== null;
  const dot = ovDotState(panel);
  const dotTitle =
    dot === "low" ? t("overview.dotLow") : dot === "ok" ? t("overview.dotOk") : t("overview.dotNone");

  let body: React.ReactNode;
  if (!panel.credential) {
    body = <p className="ov-hint">{t("empty.noCredential")}</p>;
  } else if (seg === "burn") {
    // 消耗视图只看本地统计（不依赖 API，配额刷新失败不影响这里）
    body = ovBurnBody(t, stats);
  } else if (!hasData && panel.error === null) {
    body = <p className="ov-hint">{t("panel.loading")}</p>;
  } else if (!hasData) {
    // 无缓存又拉取失败：只显示失败提示（完整错误，小字红）
    body = <p className="ov-error">{panel.error}</p>;
  } else if (isDeepSeek && balance !== null) {
    body = (
      <div className="ov-balance">
        <span className="ov-balance-num">{formatMoney(balance.currency, balance.total_balance)}</span>
        <span className="ov-balance-currency">{balance.currency}</span>
        <span className="ov-balance-status">
          {balance.is_available ? t("deepseek.statusOk") : t("deepseek.statusUnavailable")}
        </span>
      </div>
    );
  } else if (quota !== null) {
    // Kimi/GLM：5 小时 / 7 天各两行——「标签 + 重置时间 + 剩 XX%」一行，迷你进度条一行
    const windows: Array<{ label: string; detail: QuotaDetail }> = [];
    if (quota.five_hour) windows.push({ label: t("overview.fiveHour"), detail: quota.five_hour });
    if (quota.weekly) windows.push({ label: t("overview.weekly"), detail: quota.weekly });
    body = (
      <div className="ov-windows">
        {windows.map(({ label, detail }) => {
          const pctRemaining = Math.min(100, Math.max(0, detail.percent_remaining));
          const pctUsed = 100 - pctRemaining;
          return (
            <div key={label} className="ov-window">
              <div className="ov-window-head">
                <span className="ov-window-label">{label}</span>
                <span className="ov-window-reset">{resetTimeText(detail.reset_time)}</span>
                <span className="ov-left-pct">{t("overview.leftPct", { pct: Math.round(pctRemaining) })}</span>
              </div>
              <div className="progress ov-progress">
                <div className="progress-fill" style={{ width: `${pctUsed}%` }} />
              </div>
            </div>
          );
        })}
      </div>
    );
  } else {
    body = null;
  }

  return (
    <button type="button" className={`ov-row${panel.low_warning ? " low" : ""}`} onClick={onOpen}>
      <span className="ov-row-head">
        <span className={`ov-dot ${dot}`} title={dotTitle} aria-label={dotTitle} />
        <span className="ov-name">{panel.account.name}</span>
        <span className={`badge provider-badge pb-${panel.account.provider}`}>
          {isDeepSeek ? "DeepSeek" : panel.account.provider === "glm" ? "GLM" : "Kimi"}
        </span>
      </span>
      {body}
      {/* 有缓存数据但本次刷新失败：数据照显示，另加一行小字失败提示（仅额度视图，消耗视图不依赖配额接口） */}
      {seg === "quota" && hasData && panel.error !== null && (
        <p className="ov-error">{t("overview.refreshFailed")}</p>
      )}
    </button>
  );
}

/** 极简模式总览小卡（两列网格一员）：状态灯 + 名称 + 一行关键数字（额度=双窗口剩余 / 消耗=今日 token / DeepSeek=余额） */
function OverviewMiniCard({
  panel,
  seg,
  stats,
  onOpen,
}: {
  panel: AccountPanel;
  seg: "quota" | "burn";
  stats: LocalUsageStats | null;
  onOpen: () => void;
}) {
  const { t } = useTranslation();
  const isDeepSeek = panel.account.provider === "deepseek";
  const quota = panel.quota;
  const balance = panel.deepseek_balance ?? null;
  const hasData = isDeepSeek ? balance !== null : quota !== null;
  const dot = ovDotState(panel);
  const dotTitle =
    dot === "low" ? t("overview.dotLow") : dot === "ok" ? t("overview.dotOk") : t("overview.dotNone");

  let body: React.ReactNode;
  if (!panel.credential) {
    body = <p className="ov-hint">{t("empty.noCredential")}</p>;
  } else if (seg === "burn") {
    body =
      stats === null || stats.last_scan_at === null ? (
        ovBurnBody(t, stats)
      ) : (
        <div className="ov-mini-nums">
          <span>
            {formatTokens(stats.today_tokens)} {t("localUsage.today")}
          </span>
          <span className="ov-mini-sub">
            {t("localUsage.yesterday", { tokens: formatTokens(stats.yesterday_tokens) })}
          </span>
        </div>
      );
  } else if (!hasData && panel.error === null) {
    body = <p className="ov-hint">{t("panel.loading")}</p>;
  } else if (!hasData) {
    body = <p className="ov-error">{panel.error}</p>;
  } else if (isDeepSeek && balance !== null) {
    body = <p className="ov-mini-money">{formatMoney(balance.currency, balance.total_balance)}</p>;
  } else if (quota !== null) {
    // 极简小卡显示剩余百分比（不显示具体额度，跨 provider 口径一致可比）；
    // 标签定宽，「剩 XX%」上下行左缘对齐
    const leftPct = (d: QuotaDetail) => Math.round(Math.min(100, Math.max(0, d.percent_remaining)));
    body = (
      <div className="ov-mini-nums">
        {quota.five_hour && (
          <span className="ov-mini-line">
            <span className="ov-mini-label">{t("overview.fiveHour")}</span>
            <span className="ov-mini-val">{t("overview.leftPct", { pct: leftPct(quota.five_hour) })}</span>
          </span>
        )}
        {quota.weekly && (
          <span className="ov-mini-line">
            <span className="ov-mini-label">{t("overview.weekly")}</span>
            <span className="ov-mini-val">{t("overview.leftPct", { pct: leftPct(quota.weekly) })}</span>
          </span>
        )}
        {!quota.five_hour && !quota.weekly && <span>{t("panel.noData")}</span>}
      </div>
    );
  } else {
    body = null;
  }

  return (
    <button type="button" className={`ov-mini${panel.low_warning ? " low" : ""}`} onClick={onOpen}>
      <span className="ov-row-head">
        <span className={`ov-dot ${dot}`} title={dotTitle} aria-label={dotTitle} />
        <span className="ov-name">{panel.account.name}</span>
      </span>
      {body}
    </button>
  );
}

/** 总览页（第一页）：页头「总览 + 额度/消耗分段」，正常模式为监控行列表、极简模式为两列小卡网格；
 *  点行/卡跳对应账号详情页 */
function OverviewPage({
  accounts,
  minimal,
  onOpen,
  localUsage,
  onBurnVisible,
}: {
  accounts: AccountPanel[];
  minimal: boolean;
  /** 打开第 i 个账号的详情页（调用方 goTo(i+1)） */
  onOpen: (index: number) => void;
  /** 各账号本地 token 统计（按账号 id 索引，消耗视图用）；未加载的账号缺 key */
  localUsage: Record<string, LocalUsageStats>;
  /** 首次切到消耗视图时触发（调用方补拉全部账号的本地统计） */
  onBurnVisible: () => void;
}) {
  const { t } = useTranslation();
  // 分段状态驻留内存：面板隐藏再打开保持上次选择，重启回「额度」
  const [seg, setSeg] = useState<"quota" | "burn">("quota");
  const switchSeg = (next: "quota" | "burn") => {
    if (next === seg) return;
    setSeg(next);
    if (next === "burn") onBurnVisible();
  };
  return (
    <section className="page">
      {/* page-body：内容流容器（fit-content 供窗口自适应测高） */}
      <div className="page-body">
      <header className="ov-head">
        <span className="ov-title">{t("overview.title")}</span>
        <div className="seg" role="group" aria-label={t("overview.title")}>
          <button
            type="button"
            className={`seg-item${seg === "quota" ? " active" : ""}`}
            aria-pressed={seg === "quota"}
            onClick={() => switchSeg("quota")}
          >
            {t("overview.segQuota")}
          </button>
          <button
            type="button"
            className={`seg-item${seg === "burn" ? " active" : ""}`}
            aria-pressed={seg === "burn"}
            onClick={() => switchSeg("burn")}
          >
            {t("overview.segBurn")}
          </button>
        </div>
      </header>
      {minimal ? (
        <div className="ov-grid">
          {accounts.map((a, i) => (
            <OverviewMiniCard
              key={a.account.id}
              panel={a}
              seg={seg}
              stats={localUsage[a.account.id] ?? null}
              onOpen={() => onOpen(i)}
            />
          ))}
        </div>
      ) : (
        accounts.map((a, i) => (
          <OverviewRow
            key={a.account.id}
            panel={a}
            seg={seg}
            stats={localUsage[a.account.id] ?? null}
            onOpen={() => onOpen(i)}
          />
        ))
      )}
      </div>
    </section>
  );
}

/** 用量面板主界面（index.html 入口）：横向翻页容器，首页总览 + 一页一个账号，像手机桌面 */
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
  // 各账号的本地 token 消耗统计（本地统计卡用，按账号 id 索引）；未加载的账号缺 key
  const [localUsageMap, setLocalUsageMap] = useState<Record<string, LocalUsageStats>>({});
  // 预设背景 id（纯 CSS 渐变 class）；null = 未选预设
  const [bgPreset, setBgPreset] = useState<string | null>(null);
  // 极简模式（settings-changed 即时切换）：只显示 7 天 / 5 小时额度条
  const [minimal, setMinimal] = useState(false);
  // 自定义背景图（kimibg:// 协议 URL）；null = 无图
  const [bgImage, setBgImage] = useState<string | null>(null);
  // 当前已加载背景（预设 + 文件名）：settings-changed 时按它判断要不要换（防每次保存都重拉图片）
  const bgRef = useRef<{ preset: string | null; image: string | null }>({ preset: null, image: null });
  // 每分钟触发一次重渲染，让重置倒计时保持新鲜
  const [, setTick] = useState(0);

  // ---- 翻页：手势驱动（拖拽 1:1 跟手 + 弹簧吸附），滚轮累积阈值，圆点直达 ----
  const [page, setPage] = useState(0);
  // page 的 ref 镜像（事件回调里读最新页码，避免闭包过期）
  const pageRef = useRef(0);
  pageRef.current = page;
  // 翻页滑动/弹簧期间屏蔽页内滚动条（压矮落在动画途中时防旧页滚动条闪现）
  const [sliding, setSliding] = useState(false);
  const reducedMotion = usePrefersReducedMotion();

  // 轨道位移状态全部在 ref 里：动画帧直写 DOM style，不经 React 重渲染（120Hz 跟手前提）
  const pagerRef = useRef<HTMLDivElement>(null);
  const trackRef = useRef<HTMLDivElement>(null);
  /** 当前位移（px，0 = 第一页；恒等于屏幕上看到的实时 transform） */
  const posRef = useRef(0);
  /** 当前速度（px/s） */
  const velRef = useRef(0);
  /** 弹簧目标位移（px） */
  const targetRef = useRef(0);
  const rafRef = useRef<number | null>(null);
  const springDampingRef = useRef(SPRING_DAMPING_DEFAULT);
  /** 拖拽手势：指针位置历史（算释放速度）+ 起点位移；capturing = 已越过 commit 阈值 */
  const dragRef = useRef<{
    pointerId: number;
    startX: number;
    startPos: number;
    capturing: boolean;
    history: Array<{ x: number; t: number }>;
  } | null>(null);
  /** 滚轮累积器（触摸板轻微滚动不得翻页：200ms 窗口内累积过阈值才翻） */
  const wheelAccumRef = useRef({ accum: 0, lastT: 0 });

  const accounts = useMemo(() => state?.accounts ?? [], [state]);
  // 页码 0 = 总览页，账号 i 在页码 i+1；面板打开默认停在总览（page 初始值 0）
  const pageCount = accounts.length + 1;
  // pageCount 的 ref 镜像（goTo 不依赖 pageCount，避免回调身份变化）
  const pageCountRef = useRef(0);
  pageCountRef.current = pageCount;

  // 内容高度自适应：当前页内容驱动窗口高度（翻页/数据到达/极简切换都会重测）
  usePanelContentHeight(page, state !== null && accounts.length > 0);

  /** 页宽 = 翻页视口宽（.page 宽 100% 与视口一致） */
  const pageWidth = useCallback(() => pagerRef.current?.clientWidth ?? 336, []);

  /** 把位移直写到轨道（绕过每帧重渲染） */
  const applyPos = useCallback((x: number) => {
    posRef.current = x;
    if (trackRef.current) trackRef.current.style.transform = `translateX(${-x}px)`;
  }, []);

  /** 手写弹簧（§4，damping ratio + response 参数化，rAF 半隐式欧拉积分）：
   *  从当前实时位移与速度起步（§3 可中断、反向平滑）；不传初速度则沿用当前速度（换目标速度混合不硬切）。
   *  初速度取绝对 px/s（§5 速度交接无接缝；本实现用绝对值不做归一）。 */
  const animateTo = useCallback(
    (targetPx: number, velocity?: number, damping: number = SPRING_DAMPING_DEFAULT) => {
      targetRef.current = targetPx;
      if (velocity !== undefined) velRef.current = velocity;
      springDampingRef.current = damping;
      setSliding(true);
      if (rafRef.current !== null) return; // 弹簧在跑：只换目标，运动保持连续
      const omega = (2 * Math.PI) / SPRING_RESPONSE;
      const k = omega * omega;
      let lastT = performance.now();
      const tick = (now: number) => {
        const dt = Math.min((now - lastT) / 1000, 0.05); // 钳制帧间隔（防后台恢复大跳）
        lastT = now;
        const c = 2 * springDampingRef.current * omega;
        const t = targetRef.current;
        const nv = velRef.current + (-k * (posRef.current - t) - c * velRef.current) * dt;
        let nx = posRef.current + nv * dt;
        // 沉降：位置与速度都足够小 → 吸附停稳，解除滚动条屏蔽
        if (Math.abs(nx - t) < 0.05 && Math.abs(nv) < 5) {
          nx = t;
          velRef.current = 0;
          applyPos(nx);
          rafRef.current = null;
          setSliding(false);
          return;
        }
        velRef.current = nv;
        applyPos(nx);
        rafRef.current = requestAnimationFrame(tick);
      };
      rafRef.current = requestAnimationFrame(tick);
    },
    [applyPos],
  );

  /** 减少动态：瞬时切页 + 200ms 淡入（切页在 opacity 0 的不可见帧完成） */
  const jumpWithFade = useCallback(
    (next: number) => {
      setPage(next);
      const x = next * pageWidth();
      velRef.current = 0;
      targetRef.current = x;
      posRef.current = x;
      const track = trackRef.current;
      if (track) {
        track.style.transition = "none";
        track.style.transform = `translateX(${-x}px)`;
        track.style.opacity = "0";
        void track.offsetHeight; // 强制 reflow：让 opacity 0 先生效，再交给 CSS 淡入
        track.style.transition = "";
        track.style.opacity = "1";
      }
      setSliding(true);
      setTimeout(() => setSliding(false), 200);
    },
    [pageWidth],
  );

  /** 翻到指定页（越界钳制：到头停不循环）；弹簧起步即当前实时位移与速度 */
  const goTo = useCallback(
    (target: number) => {
      const next = Math.max(0, Math.min(target, Math.max(0, pageCountRef.current - 1)));
      if (reducedMotion) {
        if (next !== pageRef.current) jumpWithFade(next);
        return;
      }
      if (next === pageRef.current && rafRef.current === null) return; // 原地不动且弹簧空闲：无事发生
      setPage(next);
      animateTo(next * pageWidth(), undefined, SPRING_DAMPING_DEFAULT);
    },
    [animateTo, jumpWithFade, pageWidth, reducedMotion],
  );

  // 卸载时停掉弹簧帧循环
  useEffect(() => {
    return () => {
      if (rafRef.current !== null) cancelAnimationFrame(rafRef.current);
    };
  }, []);

  // 账号增删后页码可能越界（如删掉最后一页）：钳回范围内并同步位移（直接吸附，不弹）
  useEffect(() => {
    if (page > pageCount - 1) {
      const next = Math.max(0, pageCount - 1);
      setPage(next);
      const x = next * pageWidth();
      targetRef.current = x;
      velRef.current = 0;
      applyPos(x);
    }
  }, [page, pageCount, applyPos, pageWidth]);

  // 滚轮翻页：纵/横向都收，delta 在 200ms 窗口内累积过阈值才翻一页（触摸板轻微滚动不翻）；
  // 弹簧在途时不叠加（防触摸板惯性连翻多页）
  const onWheel = useCallback(
    (e: React.WheelEvent) => {
      if (pageCountRef.current <= 1) return;
      const d = Math.abs(e.deltaX) > Math.abs(e.deltaY) ? e.deltaX : e.deltaY;
      const now = performance.now();
      const w = wheelAccumRef.current;
      if (now - w.lastT > 200) w.accum = 0;
      w.lastT = now;
      if (rafRef.current !== null) {
        w.accum = 0;
        return;
      }
      w.accum += d;
      if (Math.abs(w.accum) < WHEEL_FLIP_ACCUM) return;
      const dir = w.accum > 0 ? 1 : -1;
      w.accum = 0;
      goTo(pageRef.current + dir);
    },
    [goTo],
  );

  // 拖拽翻页（§2 直接操纵）：越过 10px commit 阈值后捕获指针、页面 1:1 跟手；
  // 边界橡皮筋（§9）；松手动量投影选落点页（§6）+ 初速度交接弹簧（§5）
  const onPointerDown = useCallback((e: React.PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    // 打断进行中的弹簧：ref 里的位移/速度就是实时值，拖拽天然从当前值起步（§3 可中断）
    if (rafRef.current !== null) {
      cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
    }
    dragRef.current = {
      pointerId: e.pointerId,
      startX: e.clientX,
      startPos: posRef.current,
      capturing: false,
      history: [{ x: e.clientX, t: performance.now() }],
    };
  }, []);

  const onPointerMove = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      const drag = dragRef.current;
      if (drag === null || e.pointerId !== drag.pointerId) return;
      const dx = e.clientX - drag.startX;
      if (!drag.capturing) {
        if (Math.abs(dx) < DRAG_COMMIT_PX) return;
        drag.capturing = true;
        e.currentTarget.setPointerCapture(e.pointerId);
        setSliding(true);
      }
      // 速度历史：只留最近 100ms 窗口（松手时的初速度从这里来）
      const now = performance.now();
      drag.history.push({ x: e.clientX, t: now });
      while (drag.history.length > 2 && now - drag.history[0].t > 100) drag.history.shift();
      if (reducedMotion) return; // 减少动态：拖拽不跟手，松手直接淡入淡出到落点页
      const width = pageWidth();
      const maxPos = (pageCountRef.current - 1) * width;
      let pos = drag.startPos - dx;
      // §9 边界橡皮筋：第一页再向右、最后一页再向左，超出部分渐进阻尼
      if (pos < 0) pos = -rubberband(-pos, width);
      else if (pos > maxPos) pos = maxPos + rubberband(pos - maxPos, width);
      applyPos(pos);
    },
    [applyPos, pageWidth, reducedMotion],
  );

  const endDrag = useCallback(() => {
    const drag = dragRef.current;
    dragRef.current = null;
    if (drag === null || !drag.capturing || pageCountRef.current <= 1) return;
    const width = pageWidth();
    // 释放速度 = 最近 100ms 位移/时间；指针方向与内容位移相反（内容跟手 = -dx）
    const h = drag.history;
    let v = 0;
    if (h.length >= 2) {
      const dt = (h[h.length - 1].t - h[0].t) / 1000;
      if (dt > 0) v = -(h[h.length - 1].x - h[0].x) / dt;
    }
    // §6 动量投影预测落点，吸附到最近页（橡皮筋越界时投影自然落回边界页）
    const projected = posRef.current + project(v);
    const targetPage = Math.max(0, Math.min(pageCountRef.current - 1, Math.round(projected / width)));
    setPage(targetPage);
    if (reducedMotion) {
      jumpWithFade(targetPage);
      return;
    }
    // §5 初速度交接（无接缝）+ §4：仅甩动释放用 0.8 阻尼允许轻微回弹，其余临界阻尼
    animateTo(targetPage * width, v, Math.abs(v) > FLICK_VELOCITY ? SPRING_DAMPING_FLICK : SPRING_DAMPING_DEFAULT);
  }, [animateTo, jumpWithFade, pageWidth, reducedMotion]);

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
        setMinimal(s.minimal_mode);
      })
      .catch(() => {
        // 设置读取失败保持系统语言，不影响面板功能
      });
    return onSettingsChanged((s) => {
      void i18n.changeLanguage(resolveLang(s.language));
      syncBackground(s.background_preset, s.background_image);
      setMinimal(s.minimal_mode);
    });
  }, [syncBackground]);

  /** 拉取某账号的历史采样（失败静默，保留旧曲线） */
  const fetchHistory = useCallback((accountId: string) => {
    getUsageHistory(accountId)
      .then((h) => setHistoryMap((m) => ({ ...m, [accountId]: h })))
      .catch(() => {});
  }, []);

  /** 拉取某账号的本地 token 统计（失败静默，保留旧数据） */
  const fetchLocalUsage = useCallback((accountId: string) => {
    getLocalUsage(accountId)
      .then((u) => setLocalUsageMap((m) => ({ ...m, [accountId]: u })))
      .catch(() => {});
  }, []);

  /** 总览切到消耗视图：补拉全部账号的本地统计（只拉没拉过的；后端扫描有节流，重复调便宜） */
  const fetchAllLocalUsage = useCallback(() => {
    accounts.forEach((a) => {
      if (localUsageMap[a.account.id] === undefined) fetchLocalUsage(a.account.id);
    });
  }, [accounts, localUsageMap, fetchLocalUsage]);

  // 手动刷新：成功用返回值整体替换；失败把错误写进当前页横幅，保留已有缓存
  const doRefresh = useCallback(async () => {
    setRefreshing(true);
    try {
      setState(await refreshNow());
    } catch (e) {
      setState((prev) => {
        if (prev === null) return prev;
        // 失败写进「当前页」账号：页码 0 是总览页不对应任何账号，跳过不写
        const idx = pageRef.current - 1;
        const cur = idx >= 0 ? prev.accounts[idx] : undefined;
        if (cur === undefined) return prev;
        const accounts = [...prev.accounts];
        accounts[idx] = { ...cur, error: String(e) };
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
      // 每次刷新成功后历史采样会增长、本地统计可能有新扫描结果：
      // 同步重拉当前页账号的趋势与本地统计（失败静默，保留旧数据）；页码 0 是总览页，跳过
      const idx = pageRef.current - 1;
      const cur = idx >= 0 ? s.accounts[idx] : undefined;
      if (cur !== undefined) {
        fetchHistory(cur.account.id);
        fetchLocalUsage(cur.account.id);
      }
    });
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
  }, [doRefresh, fetchHistory, fetchLocalUsage]);

  // 翻页后按需补拉该账号的历史采样（还没拉过的话）；页码 0 是总览页没有当前账号，accounts[-1] 为 undefined 自然跳过
  useEffect(() => {
    const cur = accounts[page - 1];
    if (cur !== undefined && historyMap[cur.account.id] === undefined) {
      fetchHistory(cur.account.id);
    }
  }, [page, accounts, historyMap, fetchHistory]);

  // 翻页后按需补拉该账号的本地 token 统计（还没拉过的话）；页码 0 是总览页没有当前账号，accounts[-1] 为 undefined 自然跳过
  useEffect(() => {
    const cur = accounts[page - 1];
    if (cur !== undefined && localUsageMap[cur.account.id] === undefined) {
      fetchLocalUsage(cur.account.id);
    }
  }, [page, accounts, localUsageMap, fetchLocalUsage]);

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

  // 底栏「更新于」：账号页显示该账号时间；总览页（page 0）取所有账号里最新的一个，全空显示「暂无数据」
  const currentFetchedAt =
    page === 0
      ? accounts.reduce<number | null>(
          (latest, a) => (a.fetched_at !== null && (latest === null || a.fetched_at > latest) ? a.fetched_at : latest),
          null,
        )
      : (accounts[Math.min(page - 1, accounts.length - 1)]?.fetched_at ?? null);
  const busy = refreshing || state.loading;
  // 有新版本且拿到发布页地址时，底栏"更新于"左侧显示更新徽标
  const updateUrl = update?.has_update ? update.release_url : null;

  return (
    <div className={panelCls} style={bgStyle}>
      {/* 横向翻页容器：滚轮累积 / 拖拽跟手+动量 / 圆点直达，到头停不循环 */}
      <div
        className="pager"
        ref={pagerRef}
        onWheel={onWheel}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={endDrag}
        onPointerCancel={endDrag}
      >
        {/* 轨道位移完全由 JS 弹簧直写 style.transform（无 style prop，React 不接管 transform） */}
        <div ref={trackRef} className={`pager-track${sliding ? " sliding" : ""}${reducedMotion ? " rm" : ""}`}>
          <OverviewPage
            accounts={accounts}
            minimal={minimal}
            onOpen={(i) => goTo(i + 1)}
            localUsage={localUsageMap}
            onBurnVisible={fetchAllLocalUsage}
          />
          {accounts.map((a) => (
            <AccountPage
              key={a.account.id}
              panel={a}
              history={historyMap[a.account.id] ?? null}
              localUsage={localUsageMap[a.account.id] ?? null}
              minimal={minimal}
              onRetry={() => void doRefresh()}
              onOpenSettings={() => void openSettings()}
            />
          ))}
        </div>
      </div>

      {/* 圆点导航：总览点（圆角方形，与账号圆点区分）+ N 个账号圆点 + 末尾「+」（点开设置页并定位到账号添加表单） */}
      <nav className="pager-dots" aria-label={t("panel.pagesAria")}>
        <button
          type="button"
          className={`dot dot-ov${page === 0 ? " active" : ""}`}
          title={t("overview.title")}
          aria-label={t("overview.title")}
          onClick={() => goTo(0)}
        />
        {accounts.map((a, i) => (
          <button
            key={a.account.id}
            type="button"
            className={`dot${i + 1 === page ? " active" : ""}${a.low_warning ? " low" : ""}`}
            title={a.account.name}
            aria-label={a.account.name}
            onClick={() => goTo(i + 1)}
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
              {/* 齿轮图标（Feather settings，装饰性，按钮已有文字标签） */}
              <svg
                viewBox="0 0 24 24"
                width="12"
                height="12"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
                aria-hidden="true"
              >
                <circle cx="12" cy="12" r="3" />
                <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
              </svg>
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
            {currentFetchedAt !== null
              ? t("panel.updatedAt", { time: formatFetchedAt(currentFetchedAt) })
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
