// 桌面悬浮球（issue #58；v2 视觉重做）：常驻桌面的 5 小时/7 天额度玻璃珠。
// 外环 = 7 天（weekly）、内环 = 5 小时（five_hour），中心数字 = 当前口径剩余百分比；
// 颜色阈值固定（>40 绿 / 15–40 琥珀 / <15 红，与告警阈值无关），色值取自 App 主题。
//
// 交互：Pointer Events 分段——按下只记起点（指针捕获），位移越过 5px 才交给系统
// 拖拽（startDragging；防止 mousedown 即拖吞掉点击）；onMoved 静止 300ms 判拖拽
// 结束 → 后端判贴边并切换球/细条形态；未起拖拽的抬起（<5px）判点击 → 打开主面板；
// 贴边细条 hover 展开、移开收回。整体不透明度随设置即时应用。
//
// 浏览器 mock（npm run dev 直开 widget.html）：
//   ?mock=empty 无数据占位态、?mock=critical 告急态、?mock=dock 贴边细条态、
//   ?mock=balance|balancedock 余额类账号态；
//   另可叠加 ?bg=light|dark 模拟壁纸目检材质（仅 mock 下生效，生产不触发）。

import { useEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { createRoot } from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useTranslation } from "react-i18next";
import "./widget.css";
import i18n, { resolveLang } from "./i18n";
import type { WidgetState } from "./types";
import {
  getSettings,
  getWidgetLayout,
  getWidgetState,
  isTauri,
  onSettingsChanged,
  onWidgetUpdated,
  openMainPanel,
  widgetDragEnded,
  widgetSetExpanded,
} from "./ipc";

/** 拖拽结束判定：onMoved 静止该毫秒数后调 widget_drag_ended（任务书拍板 300ms） */
const DRAG_SETTLE_MS = 300;
/** 点击 vs 拖拽：mouseup 时屏幕位移小于该像素判点击 */
const CLICK_MAX_DRAG_PX = 5;
/** 告急阈值：剩余低于它弧光呼吸（固定值，与告警设置无关） */
const CRITICAL_PCT = 15;
/** 两端圆帽弧的最小可见量（约 3°）：剩余极低时也要看得见一截，不假装断了 */
const MIN_ARC_DEG = 3;

/** 交互阶段：idle 静止 / press 按下未动 / drag 拖拽中（贴边判定完成前保持） */
type Phase = "idle" | "press" | "drag";

/** 空占位数据（拿不到 state 时的兜底渲染，同无数据态） */
const EMPTY_STATE: WidgetState = {
  has_data: false,
  account_name: "",
  five_hour_pct: null,
  weekly_pct: null,
  center_pct: null,
  balance: null,
  balance_currency: null,
  balance_pct: null,
};

/** 剩余百分比 → 颜色（固定阈值，与告警设置无关；无数据灰） */
function pctColor(pct: number | null): string {
  if (pct === null) return "rgba(255,255,255,0.32)";
  if (pct > 40) return "#9ece6a";
  if (pct >= CRITICAL_PCT) return "#e0af68";
  return "#f7768e";
}

/** #rrggbb + 两位透明度 → #rrggbbaa（SVG 内联 glow / 文字辉光用） */
function withAlpha(hex: string, alpha: string): string {
  return `${hex}${alpha}`;
}

/** 余额健康度 → 颜色：绿 ≥2 倍告警线 / 琥珀 1–2 倍（接近告警线）/ 红 < 1 倍（已低于） */
function balanceColor(pct: number): string {
  if (pct >= 200) return "#9ece6a";
  if (pct >= 100) return "#e0af68";
  return "#f7768e";
}

/** 余额金额 → 球心文本（概览量级：≥10 取整、<10 留一位小数；精确值在悬停提示与面板里） */
function formatBalanceAmount(balance: number): string {
  if (!Number.isFinite(balance)) return "--";
  if (Math.abs(balance) >= 10) return String(Math.round(balance));
  return (Math.round(balance * 10) / 10).toFixed(1);
}

/** 币种符号：CNY/RMB → ¥、USD → $、其余原样（未知币种直接显示代码） */
function currencySymbol(currency: string | null): string {
  const c = (currency ?? "CNY").toUpperCase();
  if (c === "CNY" || c === "RMB") return "¥";
  if (c === "USD") return "$";
  return c;
}

/** 余额环/柱的填充量（0–100）：健康度 200（2 倍告警线）= 满环，100（正好在告警线）= 半环 */
function balanceFill(pct: number): number {
  return Math.max(0, Math.min(100, pct / 2));
}

/**
 * 中心数字的归属标签：数字口径来自哪一项（"5h" / "7d"，两项同值 "5h·7d"）。
 * 前端无从得知设置里的 center_metric，按数值反推：等于 five_hour 归 5h、
 * 等于 weekly 归 7d、都等（或都对不上）归 "5h"（auto 口径 5h 优先的既有约定）。
 */
function centerMetricLabel(state: WidgetState): string | null {
  const c = state.center_pct;
  if (c === null) return null;
  const is = (v: number | null) => v !== null && Math.abs(v - c) < 0.5;
  if (is(state.five_hour_pct) && is(state.weekly_pct)) return "5h·7d";
  if (is(state.weekly_pct)) return "7d";
  return "5h";
}

/** 双环之一：底轨 + 按剩余百分比画弧（0–100 clamp，从顶部顺时针，圆帽）。
 *  color 缺省按剩余阈值取色；余额模式传自定义色（健康度带） */
function Ring({
  pct,
  r,
  stroke,
  color,
}: {
  pct: number | null;
  r: number;
  stroke: number;
  color?: string;
}) {
  const c = 2 * Math.PI * r;
  const frac = pct === null ? 0 : Math.max(0, Math.min(100, pct)) / 100;
  const minFrac = MIN_ARC_DEG / 360;
  const shown = pct === null || pct <= 0 ? 0 : Math.max(frac, minFrac);
  const arc = color ?? pctColor(pct);
  return (
    <>
      <circle cx={38} cy={38} r={r} fill="none" stroke="var(--w-track)" strokeWidth={stroke} />
      {shown > 0 && (
        <circle
          cx={38}
          cy={38}
          r={r}
          fill="none"
          stroke={arc}
          strokeWidth={stroke}
          strokeLinecap="round"
          strokeDasharray={`${c * shown} ${c}`}
          transform="rotate(-90 38 38)"
          style={{ filter: `drop-shadow(0 0 2.4px ${withAlpha(arc, "80")})` }}
          className={color === undefined && pct !== null && pct < CRITICAL_PCT ? "ring-crit" : undefined}
        />
      )}
    </>
  );
}

/** 自由态：玻璃珠（双环 + 中央数字栈；占位态灰数字 "--"、无口径标）。
 *  余额类账号（DeepSeek）：外环按「距告警线的比例」填充、内环只留底轨，
 *  中心显示余额金额 + 币种符号 */
function Ball({ state }: { state: WidgetState }) {
  const { t } = useTranslation();
  const hasBalance = state.balance_pct !== null && state.balance !== null;
  const center = state.center_pct;
  const numText = hasBalance
    ? formatBalanceAmount(state.balance as number)
    : center === null
      ? "--"
      : `${Math.round(center)}%`;
  const unit = hasBalance
    ? `${currencySymbol(state.balance_currency)} ${t("widget.balance")}`
    : centerMetricLabel(state);
  const accent = hasBalance ? balanceColor(state.balance_pct as number) : center === null ? null : pctColor(center);
  const numColor = accent ?? "var(--w-faint)";
  const numClass =
    numText.length > 4
      ? "ball-num ball-num--xwide"
      : numText.length > 3
        ? "ball-num ball-num--wide"
        : "ball-num";
  return (
    <div className="ball">
      <svg viewBox="0 0 76 76" aria-hidden>
        <defs>
          {/* 环心柔光凹槽：把数字从环面「压」进玻璃里，边缘无硬边不冲突内环 */}
          <radialGradient id="w-well">
            <stop offset="0%" stopColor="#06070c" stopOpacity="0.62" />
            <stop offset="72%" stopColor="#06070c" stopOpacity="0.45" />
            <stop offset="100%" stopColor="#06070c" stopOpacity="0" />
          </radialGradient>
        </defs>
        <circle cx={38} cy={38} r={21} fill="url(#w-well)" />
        {hasBalance ? (
          <>
            <Ring pct={balanceFill(state.balance_pct as number)} r={32} stroke={4.4} color={accent as string} />
            <Ring pct={null} r={21.5} stroke={4.4} />
          </>
        ) : (
          <>
            <Ring pct={state.weekly_pct} r={32} stroke={4.4} />
            <Ring pct={state.five_hour_pct} r={21.5} stroke={4.4} />
          </>
        )}
      </svg>
      <div className="ball-core">
        <span
          className={numClass}
          style={{
            color: numColor,
            textShadow: accent === null ? undefined : `0 0 9px ${withAlpha(accent, "59")}`,
          }}
        >
          {numText}
        </span>
        {unit !== null && (
          <span className="ball-unit" style={{ color: accent === null ? undefined : withAlpha(accent, "a6") }}>
            {unit}
          </span>
        )}
      </div>
    </div>
  );
}

/** 细条内的一根微柱：底轨 + 从底按剩余填充的色柱（颜色给扫读、柱高给精确量）。
 *  color 缺省按剩余阈值取色；余额模式传自定义色（健康度带） */
function Gauge({ pct, color }: { pct: number | null; color?: string }) {
  const arc = color ?? pctColor(pct);
  const h = pct === null ? 0 : Math.max(0, Math.min(100, pct));
  return (
    <div className="gauge">
      {h > 0 && (
        <i
          className={color === undefined && pct !== null && pct < CRITICAL_PCT ? "crit" : undefined}
          style={{ height: `${h}%`, background: arc, boxShadow: `0 0 6px ${withAlpha(arc, "66")}` }}
        />
      )}
    </div>
  );
}

/** 贴边细条态（窗口 12×88）：上柱 5 小时、下柱 7 天；余额类账号一根居中柱
 *  （按距告警线的比例填充） */
function Strip({ state }: { state: WidgetState }) {
  if (state.balance_pct !== null) {
    return (
      <div className="strip strip-single">
        <Gauge pct={balanceFill(state.balance_pct)} color={balanceColor(state.balance_pct)} />
      </div>
    );
  }
  return (
    <div className="strip">
      <Gauge pct={state.five_hour_pct} />
      <Gauge pct={state.weekly_pct} />
    </div>
  );
}

/** 悬浮球主组件（widget.html 入口） */
function WidgetApp() {
  const { t } = useTranslation();
  const [state, setState] = useState<WidgetState | null>(null);
  const [dockEdge, setDockEdge] = useState<string | null>(null);
  const [expanded, setExpanded] = useState(false);
  const [opacity, setOpacity] = useState(1);
  const [phase, setPhase] = useState<Phase>("idle");

  // 拖拽过程状态（ref 直写不触发渲染）：mousedown 记屏幕起点，mouseup 按位移判点击
  const dragStartRef = useRef<{ x: number; y: number } | null>(null);
  const draggingRef = useRef(false);
  const settleTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // 拖拽静止 300ms → 后端判贴边/切形态/落盘；返回值同步本地 dockEdge 并结束拖拽反馈
  const finishDrag = () => {
    if (settleTimerRef.current !== null) {
      clearTimeout(settleTimerRef.current);
    }
    settleTimerRef.current = setTimeout(async () => {
      settleTimerRef.current = null;
      try {
        const edge = await widgetDragEnded();
        setDockEdge(edge);
        setExpanded(false);
      } catch {
        // 判定失败保持当前形态，下次拖拽结束后再试
      }
      draggingRef.current = false;
      setPhase("idle");
    }, DRAG_SETTLE_MS);
  };

  // 挂载：首屏数据 + 布局 + 不透明度/语言；订阅 widget-updated 与 settings-changed
  useEffect(() => {
    let alive = true;
    void getWidgetState()
      .then((s) => {
        if (alive) setState(s);
      })
      .catch(() => {
        if (alive) setState(EMPTY_STATE);
      });
    void getWidgetLayout()
      .then((edge) => {
        if (alive) setDockEdge(edge);
      })
      .catch(() => {});
    void getSettings()
      .then((s) => {
        if (!alive) return;
        setOpacity(s.widget_opacity);
        void i18n.changeLanguage(resolveLang(s.language));
      })
      .catch(() => {});
    const unWidget = onWidgetUpdated((s) => setState(s));
    const unSettings = onSettingsChanged((s) => {
      setOpacity(s.widget_opacity);
      void i18n.changeLanguage(resolveLang(s.language));
    });
    return () => {
      alive = false;
      unWidget();
      unSettings();
      if (settleTimerRef.current !== null) clearTimeout(settleTimerRef.current);
    };
  }, []);

  // 仅浏览器 mock：?bg=light|dark 模拟壁纸、?mock=dock 复现真实的 12×88 贴边窗口尺寸，
  // 目检材质在深浅背景上的可读性（均只在 mock 下生效，生产不触发）
  useEffect(() => {
    if (isTauri) return;
    const params = new URLSearchParams(window.location.search);
    const bg = params.get("bg");
    if (bg === "light" || bg === "dark") {
      document.documentElement.classList.add(`wb-${bg}`);
    }
    if (params.get("mock") === "dock") {
      document.documentElement.classList.add("wb-dockwin");
    }
  }, []);

  // 窗口事件（仅 Tauri 运行时）：onMoved 拖拽期切换拖拽反馈并重置静止判定
  useEffect(() => {
    if (!isTauri) return;
    const win = getCurrentWindow();
    // onMoved 注册是异步的：注册完成前卸载用 cancelled 标记兜底反注册（同 ipc.ts 模式）
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    void win
      .onMoved(() => {
        if (!draggingRef.current) return;
        setPhase("drag");
        finishDrag();
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  /** 按下（左键）：记屏幕起点、进按压态、抓住指针（窗口外也能收到移动/抬起）。
   *  这里**不起**系统拖拽——mousedown 就 startDragging 会把后续交给系统移动循环，
   *  mouseup 再也回不到 webview，点击判不出来（2026-10-02 实机抓到：点击不开面板） */
  const onPointerDown = (e: ReactPointerEvent) => {
    if (e.button !== 0) return;
    dragStartRef.current = { x: e.screenX, y: e.screenY };
    setPhase("press");
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      // 捕获失败不致命：指针基本都在球内活动，光靠普通事件也能触发拖拽
    }
  };

  /** 按住移动：位移越过阈值（5px，同点击判定）才把窗口交给系统拖拽——
   *  给点击留出抖动余量，也保证「按下即拖」与「按下即点」两条路互不打架 */
  const onPointerMove = (e: ReactPointerEvent) => {
    const start = dragStartRef.current;
    if (start === null || draggingRef.current || e.buttons === 0) return;
    const dx = e.screenX - start.x;
    const dy = e.screenY - start.y;
    if (dx * dx + dy * dy <= CLICK_MAX_DRAG_PX * CLICK_MAX_DRAG_PX) return;
    draggingRef.current = true;
    setPhase("drag");
    if (isTauri) void getCurrentWindow().startDragging();
  };

  /** 抬起：未起拖拽 = 点击 → 打开主面板；已起拖拽则由 onMoved 静止判定收尾 */
  const onPointerUp = (e: ReactPointerEvent) => {
    const start = dragStartRef.current;
    dragStartRef.current = null;
    if (e.button !== 0 || start === null) return;
    if (draggingRef.current) return;
    // 点击：取消在途的拖拽结束判定，回到静止态并开面板
    if (settleTimerRef.current !== null) {
      clearTimeout(settleTimerRef.current);
      settleTimerRef.current = null;
    }
    setPhase("idle");
    openMainPanel();
  };

  /** 指针取消（系统抢走指针等）：清干净状态，不判点击 */
  const onPointerCancel = () => {
    dragStartRef.current = null;
    if (!draggingRef.current) setPhase("idle");
  };

  /** 细条 hover 展开（后端变 96×96 并内移对齐贴边） */
  const onEnter = () => {
    if (dockEdge !== null && !expanded) {
      setExpanded(true);
      void widgetSetExpanded(true);
    }
  };
  /** 细条移开：后端按真实光标位置决定收不收（窗口缩放瞬间 WebView2 会合成一次
   *  mouseleave 而指针没动，后端会忽略它）——只有真收起了才同步本地形态，防闪回 */
  const onLeave = () => {
    if (dockEdge !== null && expanded) {
      void widgetSetExpanded(false).then((collapsed) => {
        if (collapsed) setExpanded(false);
      });
    }
  };

  const data = state ?? EMPTY_STATE;
  const docked = dockEdge !== null && !expanded;

  // 悬停提示：账号名 + 两项剩余明细；无数据时与面板同款占位文案
  const tip = useMemo(() => {
    if (!data.has_data) return t("panel.noData");
    const parts: string[] = [data.account_name];
    if (data.balance !== null && data.balance_currency !== null) {
      parts.push(
        t("widget.tipBalance", {
          amount: `${currencySymbol(data.balance_currency)}${data.balance.toFixed(2)}`,
        }),
      );
    }
    if (data.five_hour_pct !== null) parts.push(t("widget.tip5h", { pct: Math.round(data.five_hour_pct) }));
    if (data.weekly_pct !== null) parts.push(t("widget.tip7d", { pct: Math.round(data.weekly_pct) }));
    return parts.filter(Boolean).join(" · ");
  }, [data, t]);

  return (
    <div
      className={
        "widget" + (docked ? " docked" : "") + (phase === "press" ? " pressed" : "") +
        (phase === "drag" ? " dragging" : "")
      }
      style={{ opacity }}
      title={tip}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerCancel}
      onMouseEnter={onEnter}
      onMouseLeave={onLeave}
    >
      {/* key 随形态切换重挂 → 重播 materialize；贴边侧决定从哪条边展开 */}
      <div
        className={"stage" + (dockEdge !== null ? ` from-${dockEdge}` : "")}
        key={docked ? "strip" : "ball"}
      >
        {docked ? (
          <Strip state={data} />
        ) : (
          <div className="cluster">
            <Ball state={data} />
            {data.has_data && data.account_name !== "" && (
              <div className="chip">{data.account_name}</div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

createRoot(document.getElementById("root") as HTMLElement).render(<WidgetApp />);
