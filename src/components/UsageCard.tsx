import { useTranslation } from "react-i18next";
import i18n from "../i18n";
import type { QuotaDetail } from "../types";

/** 本地时间补零格式化：dateOnly → 2026-08-20（月度，同官网），否则 08-03 12:56 */
function formatResetTime(d: Date, dateOnly: boolean): string {
  const p = (n: number) => String(n).padStart(2, "0");
  const date = `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
  if (dateOnly) return date;
  return `${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

/**
 * 重置时间文案（官网风格绝对时间）：滚动窗口"08-03 12:56 后重置"、
 * 月度 dateOnly 时"2026-08-20 后重置"；不足 1 分钟（含已过期）显示"即将重置"，
 * reset_time 缺失或无法解析显示"重置时间未知"（英文 "Reset: unknown"）。
 * 导出供 MonthlyCard 复用。直接走 i18n 单例（调用方均为 render 期执行，语言切换会触发重渲染）。
 */
export function resetTimeText(resetTime?: string, dateOnly = false): string {
  if (!resetTime) return i18n.t("usage.countdown.unknown");
  const at = new Date(resetTime);
  const diffMs = at.getTime() - Date.now();
  if (Number.isNaN(diffMs)) return i18n.t("usage.countdown.unknown");
  if (diffMs < 60_000) return i18n.t("usage.countdown.soon");
  return i18n.t("usage.countdown.at", { time: formatResetTime(at, dateOnly) });
}

interface UsageCardProps {
  /** 卡片标题，如"7 天用量" */
  title: string;
  detail: QuotaDetail;
}

/** 单个时间窗口的用量卡：大字号已用百分比 + 进度条 + 剩余量 + 重置倒计时 */
export function UsageCard({ title, detail }: UsageCardProps) {
  const { t } = useTranslation();
  // 告警判定不受显示语义影响：剩余低于 20% 整体标红
  const low = detail.percent_remaining < 20;
  const pctRemaining = Math.min(100, Math.max(0, detail.percent_remaining));
  const pctUsed = 100 - pctRemaining;
  return (
    <div className={`pcard usage-card${low ? " low" : ""}`}>
      <div className="usage-head">
        <span className="usage-title">{title}</span>
        <span className="usage-pct">{Math.round(pctUsed)}%</span>
      </div>
      <div className="progress">
        <div className="progress-fill" style={{ width: `${pctUsed}%` }} />
      </div>
      <div className="usage-foot">
        <span>{t("usage.remaining", { remaining: detail.remaining, limit: detail.limit })}</span>
        <span>{resetTimeText(detail.reset_time)}</span>
      </div>
    </div>
  );
}
