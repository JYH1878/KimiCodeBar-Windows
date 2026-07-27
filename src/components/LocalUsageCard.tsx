import { useTranslation } from "react-i18next";
import type { DailyUsage, LocalUsageStats } from "../types";

// 柱状图几何（viewBox 0 0 320 44）：7 根柱，柱宽 28、间距 16，水平居中
const BAR_W = 28;
const BAR_GAP = 16;
const CHART_W = 320;
const CHART_H = 44;
const BASE_Y = 40;
const MAX_BAR_H = 34;

/**
 * token 数紧凑格式化：≥1e6 → "1.2M"，≥1000 → "12.8K"，否则原样。
 * 中英文案通用（K/M 缩写两种语言下含义一致）。
 */
export function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
  return String(n);
}

/** 模型名取斜杠后短名（"kimi-code/k3" → "k3"），无斜杠原样返回 */
function shortModelName(model: string): string {
  const idx = model.lastIndexOf("/");
  return idx >= 0 ? model.slice(idx + 1) : model;
}

interface LocalUsageCardProps {
  /** 本地统计；null = 尚未加载（不渲染，避免卡片跳动） */
  stats: LocalUsageStats | null;
}

/**
 * 本地 Token 消耗卡（扫描 wire.jsonl 的纯本地统计，不依赖 API）：
 * 今日消耗大字 + 昨日小字 + 近 7 天迷你柱状图（今日柱满不透明高亮）
 * + 按模型占比一行小字。last_scan_at 为空（从未扫描）时整卡不渲染。
 */
export function LocalUsageCard({ stats }: LocalUsageCardProps) {
  const { t } = useTranslation();
  if (stats === null || stats.last_scan_at === null) return null;

  // 近 7 天（升序，末位今日）；不足 7 天时左侧补零值占位，保持 7 根柱对齐
  const days: DailyUsage[] = stats.daily.slice(-7);
  while (days.length < 7) days.unshift({ date: "", tokens: 0 });
  const maxTokens = Math.max(1, ...days.map((d) => d.tokens));
  const totalW = days.length * BAR_W + (days.length - 1) * BAR_GAP;
  const startX = (CHART_W - totalW) / 2;

  // 按模型占比行：全部时间为零时不展示
  const modelTotal = stats.by_model.reduce((sum, m) => sum + m.tokens, 0);
  const modelLine =
    modelTotal > 0
      ? stats.by_model
          .map((m) => `${shortModelName(m.model)} ${Math.round((m.tokens / modelTotal) * 100)}%`)
          .join(" · ")
      : null;

  return (
    <div className="pcard local-usage-card">
      <div className="usage-head">
        <span className="usage-title">{t("localUsage.title")}</span>
        <span className="local-yesterday">
          {t("localUsage.yesterday", { tokens: formatTokens(stats.yesterday_tokens) })}
        </span>
      </div>
      <div className="local-today-row">
        <span className="local-tokens">{formatTokens(stats.today_tokens)}</span>
        <span className="local-today-label">{t("localUsage.today")}</span>
      </div>
      <svg
        className="local-bars"
        viewBox={`0 0 ${CHART_W} ${CHART_H}`}
        role="img"
        aria-label={t("localUsage.ariaLabel")}
      >
        {days.map((d, i) => {
          // 有消耗时按最大值比例缩放（保底 2px 可见），零消耗不画柱（纯事实）
          const h = d.tokens > 0 ? Math.max(2, (d.tokens / maxTokens) * MAX_BAR_H) : 0;
          const x = startX + i * (BAR_W + BAR_GAP);
          return (
            <rect
              key={d.date || `pad-${i}`}
              className={`usage-bar${i === days.length - 1 ? " today" : ""}`}
              x={x}
              y={BASE_Y - h}
              width={BAR_W}
              height={h}
              rx={3}
            >
              <title>{`${d.date}: ${formatTokens(d.tokens)}`}</title>
            </rect>
          );
        })}
        {/* 基线 */}
        <line
          x1={startX - 4}
          x2={startX + totalW + 4}
          y1={BASE_Y}
          y2={BASE_Y}
          stroke="var(--border)"
          strokeWidth={1}
        />
      </svg>
      {modelLine !== null && <p className="local-models">{modelLine}</p>}
    </div>
  );
}
