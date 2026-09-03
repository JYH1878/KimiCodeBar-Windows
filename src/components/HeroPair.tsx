import { useTranslation } from "react-i18next";
import type { QuotaDetail } from "../types";
import { resetTimeText } from "./UsageCard";

interface HeroPairProps {
  /** 7 天窗口；可能缺失 */
  weekly?: QuotaDetail;
  /** 5 小时窗口；可能缺失 */
  fiveHour?: QuotaDetail;
}

/** Hero 双联卡：7 天 / 5 小时两个速率窗口并置，详情页首屏的核心数字。
 *  单窗口缺失时只渲染存在的一张（自动占满整行）；都缺失时整组不渲染。 */
export function HeroPair({ weekly, fiveHour }: HeroPairProps) {
  const { t } = useTranslation();
  if (!weekly && !fiveHour) return null;
  return (
    <div className="hero-row">
      {fiveHour && <HeroCard title={t("panel.fiveHourUsage")} detail={fiveHour} />}
      {weekly && <HeroCard title={t("panel.weeklyUsage")} detail={weekly} />}
    </div>
  );
}

/** 单个窗口的 hero 卡：等宽大号已用百分比 + 进度条 + 剩余量/重置倒计时两行小字。
 *  告警判定与 UsageCard 一致：剩余低于 20% 数字与进度条整体标红。 */
function HeroCard({ title, detail }: { title: string; detail: QuotaDetail }) {
  const { t } = useTranslation();
  const low = detail.percent_remaining < 20;
  const pctRemaining = Math.min(100, Math.max(0, detail.percent_remaining));
  const pctUsed = 100 - pctRemaining;
  return (
    <div className={`pcard hero-card${low ? " low" : ""}`}>
      <div className="hero-title">{title}</div>
      <div className="hero-pct">{Math.round(pctUsed)}%</div>
      <div className="progress hero-progress">
        <div className="progress-fill" style={{ width: `${pctUsed}%` }} />
      </div>
      <div className="hero-foot">
        <span>{t("usage.remaining", { remaining: detail.remaining, limit: detail.limit })}</span>
        <span>{resetTimeText(detail.reset_time)}</span>
      </div>
    </div>
  );
}
