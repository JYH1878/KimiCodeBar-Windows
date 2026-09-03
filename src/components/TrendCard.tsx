import { useId } from "react";
import { useTranslation } from "react-i18next";
import type { HistoryPoint } from "../types";

/** 趋势窗口：近 24 小时（秒） */
const WINDOW_SEC = 24 * 3600;

// SVG 绘图区几何（viewBox 0 0 320 64，矮身定高）：图例上移到卡片头行，绘图区占满卡宽
const PLOT_L = 4;
const PLOT_R = 316;
const PLOT_T = 4;
const PLOT_B = 60;

/** 折线配色（引用 CSS 变量，随主题切换）：7 天窗口蓝 / 5 小时窗口紫 */
const WEEKLY_COLOR = "var(--accent)";
const FIVE_HOUR_COLOR = "var(--accent-2)";

/** 单个可绘制点（SVG 坐标） */
interface PlotPoint {
  x: number;
  y: number;
}

interface TrendCardProps {
  /** 历史采样点（"已用"百分比语义）；null = 尚未加载 */
  points: HistoryPoint[] | null;
}

/**
 * 用量趋势卡：纯手写 SVG 折线图（不引任何图表库，内存红线）。
 * 画近 24 小时内 7 天窗口（蓝）与 5 小时窗口（紫）两条"已用百分比"折线，
 * 线下垫 14%→0 面积渐变（稀疏数据也有视觉锚点，不再是空图）、每条序列末端画当前值圆点；
 * 图例在卡片头行右侧，不占绘图区。
 * x 轴按时间线性映射（右端即"现在"），y 轴 0-100%，只画 0%/50%/100%
 * 三条参考虚线不画刻度数字；缺失点与离线空档（相邻采样 >30 分钟）
 * 在折线上断开分段，不做任何插值或预测。
 * 可见数据点不足 2 个时不渲染图表，只显示"数据积累中…"占位。
 */
export function TrendCard({ points }: TrendCardProps) {
  const { t } = useTranslation();
  // 渐变 id 加组件实例前缀：多账号页同时挂载多张趋势卡，避免跨 SVG 的 id 冲突
  const uid = useId();
  // 只取最近 24 小时内的采样（纯事实：窗口外的一律不画）
  const nowSec = Math.floor(Date.now() / 1000);
  const startSec = nowSec - WINDOW_SEC;
  const visible = (points ?? []).filter((p) => p.t >= startSec);

  // 时间 → x 坐标：固定 24 小时窗口线性映射，越界防御性钳制
  const toX = (t: number): number => {
    const ratio = Math.min(1, Math.max(0, (t - startSec) / WINDOW_SEC));
    return PLOT_L + ratio * (PLOT_R - PLOT_L);
  };
  // 已用百分比 → y 坐标：0% 在底部、100% 在顶部，异常值钳制到 0-100
  const toY = (pct: number): number => {
    const clamped = Math.min(100, Math.max(0, pct));
    return PLOT_B - (clamped / 100) * (PLOT_B - PLOT_T);
  };

  /** 断线阈值：相邻采样间隔超过 30 分钟视为离线空档（轮询间隔为分钟级）。
   *  空档两端不连线——没数据的地方就不该有线（纯事实，不做任何插值） */
  const GAP_SEC = 30 * 60;

  /** 把一条序列按缺失点（null/undefined）与离线空档切成若干连续段，段内保持时间顺序 */
  const buildSegments = (
    pick: (p: HistoryPoint) => number | null | undefined,
  ): PlotPoint[][] => {
    const segs: PlotPoint[][] = [];
    let current: PlotPoint[] = [];
    let prevT: number | null = null;
    for (const p of visible) {
      const v = pick(p);
      const isGap = prevT !== null && p.t - prevT > GAP_SEC;
      if (v === null || v === undefined || isGap) {
        if (current.length > 0) segs.push(current);
        current = [];
      }
      if (v !== null && v !== undefined) {
        current.push({ x: toX(p.t), y: toY(v) });
      }
      prevT = p.t;
    }
    if (current.length > 0) segs.push(current);
    return segs;
  };

  /** 段的面积填充路径：沿线走到右端点，垂直落到底边再沿底边闭合（孤立单点无面积，不画） */
  const areaPath = (seg: PlotPoint[]): string => {
    const line = seg.map((p, i) => `${i === 0 ? "M" : "L"}${p.x.toFixed(1)},${p.y.toFixed(1)}`).join(" ");
    const last = seg[seg.length - 1];
    return `${line} L${last.x.toFixed(1)},${PLOT_B} L${seg[0].x.toFixed(1)},${PLOT_B} Z`;
  };

  /** 序列最后一个非空段的末点（"当前值"圆点位置） */
  const lastPoint = (segs: PlotPoint[][]): PlotPoint | null => {
    for (let i = segs.length - 1; i >= 0; i--) {
      if (segs[i].length > 0) return segs[i][segs[i].length - 1];
    }
    return null;
  };

  /** 渲染一条序列：面积填充（仅 ≥2 点段）→ 折线（孤立单点只画小圆点，不脑补连线）→ 末端当前值圆点 */
  const renderSeries = (segs: PlotPoint[][], color: string, fill: string) => {
    const last = lastPoint(segs);
    return (
      <>
        {segs
          .filter((seg) => seg.length >= 2)
          .map((seg, i) => (
            <path key={`a${i}`} d={areaPath(seg)} fill={fill} stroke="none" />
          ))}
        {segs.map((seg, i) =>
          seg.length >= 2 ? (
            <polyline
              key={`l${i}`}
              points={seg.map((p) => `${p.x.toFixed(1)},${p.y.toFixed(1)}`).join(" ")}
              fill="none"
              stroke={color}
              strokeWidth={2}
              strokeLinejoin="round"
              strokeLinecap="round"
            />
          ) : (
            <circle key={`l${i}`} cx={seg[0].x} cy={seg[0].y} r={1.5} fill={color} />
          ),
        )}
        {last !== null && <circle cx={last.x} cy={last.y} r={2.2} fill={color} stroke="var(--card-bg)" strokeWidth={1} />}
      </>
    );
  };

  const weeklySegs = buildSegments((p) => p.weekly);
  const fiveHourSegs = buildSegments((p) => p.five_hour);

  return (
    <div className="pcard trend-card">
      <div className="usage-head">
        <span className="usage-title">{t("trend.title")}</span>
        {/* 图例上移到卡片头行（不占绘图区）：色块 + 小字 */}
        <span className="trend-legend">
          <span className="trend-legend-item">
            <span className="trend-chip" style={{ background: WEEKLY_COLOR }} />
            {t("trend.legend7d")}
          </span>
          <span className="trend-legend-item">
            <span className="trend-chip" style={{ background: FIVE_HOUR_COLOR }} />
            {t("trend.legend5h")}
          </span>
        </span>
      </div>
      {visible.length < 2 ? (
        <p className="trend-empty">{t("trend.empty")}</p>
      ) : (
        <svg
          className="trend-chart"
          viewBox="0 0 320 64"
          role="img"
          aria-label={t("trend.ariaLabel")}
        >
          <defs>
            {/* 线下面积渐变：系列色 14% → 0（自上而下淡出） */}
            <linearGradient id={`${uid}-weekly`} x1="0" y1="0" x2="0" y2="1">
              <stop offset="0" stopColor={WEEKLY_COLOR} stopOpacity={0.14} />
              <stop offset="1" stopColor={WEEKLY_COLOR} stopOpacity={0} />
            </linearGradient>
            <linearGradient id={`${uid}-fiveh`} x1="0" y1="0" x2="0" y2="1">
              <stop offset="0" stopColor={FIVE_HOUR_COLOR} stopOpacity={0.14} />
              <stop offset="1" stopColor={FIVE_HOUR_COLOR} stopOpacity={0} />
            </linearGradient>
          </defs>
          {/* 0% / 50% / 100% 三条参考虚线（不画刻度数字） */}
          {[0, 50, 100].map((pct) => (
            <line
              key={pct}
              className="trend-ref"
              x1={PLOT_L}
              x2={PLOT_R}
              y1={toY(pct)}
              y2={toY(pct)}
            />
          ))}
          {renderSeries(weeklySegs, WEEKLY_COLOR, `url(#${uid}-weekly)`)}
          {renderSeries(fiveHourSegs, FIVE_HOUR_COLOR, `url(#${uid}-fiveh)`)}
        </svg>
      )}
    </div>
  );
}
