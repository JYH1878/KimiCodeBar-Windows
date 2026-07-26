import type { MonthlyInfo } from "../types";
import { resetCountdownText } from "./UsageCard";

interface MonthlyCardProps {
  /** 月度总量数据（百分比为"已用"语义，与 UsageCard 的"剩余"语义相反） */
  monthly: MonthlyInfo;
}

/** 月度总量卡：大字号总已用百分比 + Kimi/Code 分段进度条 + 分项占比与重置倒计时 */
export function MonthlyCard({ monthly }: MonthlyCardProps) {
  // 防御性钳制：分段宽度不为负、合计不超过 100%，避免异常数据撑破进度条
  const total = Math.min(100, Math.max(0, monthly.total_pct));
  const kimi = Math.min(Math.max(0, monthly.kimi_pct), total);
  const code = Math.min(Math.max(0, monthly.code_pct), total - kimi);
  return (
    <div className="pcard monthly-card">
      <div className="usage-head">
        <span className="usage-title">月度总量</span>
        <span className="usage-pct">{total.toFixed(1)}%</span>
      </div>
      <div className="progress monthly-progress">
        <div className="seg-kimi" style={{ width: `${kimi}%` }} />
        <div className="seg-code" style={{ width: `${code}%` }} />
      </div>
      <div className="usage-foot">
        <span>
          Kimi {monthly.kimi_pct.toFixed(1)}% · Code {monthly.code_pct.toFixed(1)}%
        </span>
        <span>重置：{resetCountdownText(monthly.reset_time)}</span>
      </div>
    </div>
  );
}
