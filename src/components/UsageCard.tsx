import { useTranslation } from "react-i18next";
import i18n from "../i18n";
import type { QuotaDetail } from "../types";

/**
 * 重置倒计时文案：x天后/x小时后/x分钟后重置；
 * 不足 1 分钟（含已过期）显示"即将重置"，reset_time 缺失或无法解析显示"未知"。
 * 导出供 MonthlyCard 复用。直接走 i18n 单例（调用方均为 render 期执行，语言切换会触发重渲染）。
 */
export function resetCountdownText(resetTime?: string): string {
  if (!resetTime) return i18n.t("usage.countdown.unknown");
  const diffMs = new Date(resetTime).getTime() - Date.now();
  if (Number.isNaN(diffMs)) return i18n.t("usage.countdown.unknown");
  if (diffMs < 60_000) return i18n.t("usage.countdown.soon");
  const minutes = Math.floor(diffMs / 60_000);
  if (minutes < 60) return i18n.t("usage.countdown.minutes", { count: minutes });
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return i18n.t("usage.countdown.hours", { count: hours });
  const days = Math.floor(hours / 24);
  return i18n.t("usage.countdown.days", { count: days });
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
        <span>{t("usage.reset", { text: resetCountdownText(detail.reset_time) })}</span>
      </div>
    </div>
  );
}
