import { useTranslation } from "react-i18next";
import type { DailyUsage, HistoryPoint, LocalUsageStats } from "../types";

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

/**
 * 已知 Kimi 模型 ID → 展示名（对应官方 Model ID，见 kimi.com/code/docs 模型配置页）。
 * 只收录与 ID 差异明显的模型；未收录的保持原样，避免对未知模型做错误映射。
 */
const MODEL_DISPLAY: Record<string, string> = {
  "kimi-for-coding": "K2.7",
  "kimi-for-coding-highspeed": "K2.7 HighSpeed",
};

/** 模型名取斜杠后短名（"kimi-code/k3" → "k3"），再查展示名映射；
 * 未收录的按 deepseek 前缀规则处理（品牌名首字母大写），其余原样返回 */
function shortModelName(model: string): string {
  const idx = model.lastIndexOf("/");
  const short = idx >= 0 ? model.slice(idx + 1) : model;
  const mapped = MODEL_DISPLAY[short];
  if (mapped !== undefined) return mapped;
  // deepseek-v4-flash → DeepSeek-v4-flash（品牌名按官方拼写 DeepSeek）
  if (short.startsWith("deepseek")) return "DeepSeek" + short.slice("deepseek".length);
  return short;
}

interface LocalUsageCardProps {
  /** 本地统计；null = 尚未加载（不渲染，避免卡片跳动） */
  stats: LocalUsageStats | null;
  /** 该账号的历史采样点（算「今日已用占周配额」用）；null/缺省 = 未加载或无历史 */
  history?: HistoryPoint[] | null;
}

/**
 * 「今日已用占周配额」纯函数（issue #38）。
 * 口径：只看**官方周额度已用%的当日增量**（额度消耗情况），与本地 token 消耗无关。
 * - 基准 = 最后一个 t 早于今日 00:00（本地时区）的点的 weekly；
 *   没有零点前样本时取今日最早点兜底（此时数字是当日下限）；
 * - 今日占比 = 最新点 weekly − 基准 weekly；若最新 < 基准（7 天窗口当天重置过）→ 取最新值本身；
 * - 今日一个点都没有 → null（不显示该行）；所有点 weekly 缺失（DeepSeek / 无套餐）→ null。
 * 返回未取整的百分数（展示层负责 1 位小数），不封顶。
 */
export function todayWeeklyQuotaPct(points: HistoryPoint[], now: Date = new Date()): number | null {
  const valid = points
    .filter((p): p is HistoryPoint & { weekly: number } => p.weekly !== null && p.weekly !== undefined)
    .sort((a, b) => a.t - b.t);
  if (valid.length === 0) return null;
  const dayStart = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime() / 1000;
  const todayIdx = valid.findIndex((p) => p.t >= dayStart);
  if (todayIdx < 0) return null; // 今日无样本
  const latest = valid[valid.length - 1].weekly;
  const baseline = todayIdx > 0 ? valid[todayIdx - 1].weekly : valid[todayIdx].weekly;
  return latest < baseline ? latest : latest - baseline;
}

/**
 * 本地 Token 消耗卡（扫描 wire.jsonl 的纯本地统计，不依赖 API）：
 * 今日消耗大字 + 昨日小字 + 近 7 天迷你柱状图（今日柱满不透明高亮）
 * + 今日分模型占比一行小字（与卡片主体同为今日窗口，非累计）
 * + 「占用周配额」小字内联在「今日」标签后（官方周额度已用%的当日增量，
 *   issue #38；周配额数据全缺失的账号——DeepSeek / 无套餐——不渲染）。
 * last_scan_at 为空（从未扫描）时整卡不渲染。
 */
export function LocalUsageCard({ stats, history }: LocalUsageCardProps) {
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

  // 「占用周配额」：周配额历史不足的账号（DeepSeek / 无套餐 / 今日无样本）为 null 不渲染
  const quotaPct = history ? todayWeeklyQuotaPct(history) : null;

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
        {quotaPct !== null && (
          <span className="local-today-quota">
            {t("localUsage.todayQuotaPct", { pct: quotaPct.toFixed(1) })}
          </span>
        )}
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
