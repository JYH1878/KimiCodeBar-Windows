import { useTranslation } from "react-i18next";
import type { HistoryPoint } from "../types";

/** 趋势窗口：近 24 小时（秒） */
const WINDOW_SEC = 24 * 3600;

// SVG 绘图区几何（viewBox 0 0 320 90）：右端留出图例区，上下各留边距
const PLOT_L = 4;
const PLOT_R = 272;
const PLOT_T = 4;
const PLOT_B = 84;

/** 折线配色（Tokyo Night）：7 天窗口蓝 / 5 小时窗口紫 */
const WEEKLY_COLOR = "#7aa2f7";
const FIVE_HOUR_COLOR = "#bb9af7";

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
 * 画近 24 小时内 7 天窗口（蓝）与 5 小时窗口（紫）两条"已用百分比"折线：
 * x 轴按时间线性映射（右端即"现在"），y 轴 0-100%，只画 0%/50%/100%
 * 三条参考虚线不画刻度数字；缺失点与离线空档（相邻采样 >30 分钟）
 * 在折线上断开分段，不做任何插值或预测。
 * 可见数据点不足 2 个时不渲染图表，只显示"数据积累中…"占位。
 */
export function TrendCard({ points }: TrendCardProps) {
  const { t } = useTranslation();
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

  /** 渲染一条折线：≥2 点的段画 polyline，孤立单点只画小圆点（不脑补连线） */
  const renderLine = (segs: PlotPoint[][], color: string) =>
    segs.map((seg, i) =>
      seg.length >= 2 ? (
        <polyline
          key={i}
          points={seg.map((p) => `${p.x.toFixed(1)},${p.y.toFixed(1)}`).join(" ")}
          fill="none"
          stroke={color}
          strokeWidth={1.5}
          strokeLinejoin="round"
          strokeLinecap="round"
        />
      ) : (
        <circle key={i} cx={seg[0].x} cy={seg[0].y} r={1.5} fill={color} />
      ),
    );

  return (
    <div className="pcard trend-card">
      <div className="usage-head">
        <span className="usage-title">{t("trend.title")}</span>
      </div>
      {visible.length < 2 ? (
        <p className="trend-empty">{t("trend.empty")}</p>
      ) : (
        <svg
          className="trend-chart"
          viewBox="0 0 320 90"
          role="img"
          aria-label={t("trend.ariaLabel")}
        >
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
          {renderLine(buildSegments((p) => p.weekly), WEEKLY_COLOR)}
          {renderLine(buildSegments((p) => p.five_hour), FIVE_HOUR_COLOR)}
          {/* 右端两行图例：色块 + 小字 */}
          <rect x={280} y={27} width={8} height={8} rx={2} fill={WEEKLY_COLOR} />
          <text className="trend-legend-text" x={292} y={34}>
            {t("trend.legend7d")}
          </text>
          <rect x={280} y={51} width={8} height={8} rx={2} fill={FIVE_HOUR_COLOR} />
          <text className="trend-legend-text" x={292} y={58}>
            {t("trend.legend5h")}
          </text>
        </svg>
      )}
    </div>
  );
}
