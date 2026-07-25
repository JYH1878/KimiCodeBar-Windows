import type { BoosterInfo } from "../types";

interface BoosterCardProps {
  booster?: BoosterInfo;
}

/** Booster 小卡：未开通显示提示；已开通显示余额与月度已用/限额（字段存在时） */
export function BoosterCard({ booster }: BoosterCardProps) {
  if (!booster || !booster.enabled) {
    return (
      <div className="pcard mini-card">
        <div className="mini-title">Booster</div>
        <div className="mini-value muted-text">未开通</div>
      </div>
    );
  }
  const used = booster.monthly_used_yuan;
  const limit = booster.monthly_charge_limit_yuan;
  return (
    <div className="pcard mini-card">
      <div className="mini-title">Booster 余额</div>
      <div className="mini-value">¥{booster.balance_yuan.toFixed(2)}</div>
      {(used != null || limit != null) && (
        <div className="mini-sub">
          月度 {used != null ? `¥${used.toFixed(2)}` : "—"}/
          {limit != null ? `¥${limit.toFixed(2)}` : "—"}
        </div>
      )}
    </div>
  );
}
