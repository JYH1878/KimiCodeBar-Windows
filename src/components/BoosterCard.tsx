import { useTranslation } from "react-i18next";
import type { BoosterInfo } from "../types";

interface BoosterCardProps {
  booster?: BoosterInfo;
}

/** Booster 小卡：未开通显示提示；已开通显示余额与月度已用/限额（字段存在时） */
export function BoosterCard({ booster }: BoosterCardProps) {
  const { t } = useTranslation();
  if (!booster || !booster.enabled) {
    return (
      <div className="pcard mini-card">
        <div className="mini-title">{t("booster.title")}</div>
        <div className="mini-value muted-text">{t("booster.notEnabled")}</div>
      </div>
    );
  }
  const used = booster.monthly_used_yuan;
  const limit = booster.monthly_charge_limit_yuan;
  return (
    <div className="pcard mini-card">
      <div className="mini-title">{t("booster.balanceTitle")}</div>
      <div className="mini-value num">¥{booster.balance_yuan.toFixed(2)}</div>
      {(used != null || limit != null) && (
        <div className="mini-sub">
          {t("booster.monthly", {
            used: used != null ? `¥${used.toFixed(2)}` : "—",
            limit: limit != null ? `¥${limit.toFixed(2)}` : "—",
          })}
        </div>
      )}
    </div>
  );
}
