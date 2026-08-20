import { useTranslation } from "react-i18next";
import type { DeepSeekBalance } from "../types";

/** 币种 → 符号（与后端 i18n::money_text 一致：CNY/USD 符号前缀，其余币种代码前缀） */
export function formatMoney(currency: string, amount: number): string {
  const text = amount.toFixed(2);
  if (currency === "CNY") return `¥${text}`;
  if (currency === "USD") return `$${text}`;
  return `${currency} ${text}`;
}

/** epoch 秒 → 本地时间 HH:mm:ss（与 panel.tsx 底栏一致） */
function formatTime(epochSec: number): string {
  const d = new Date(epochSec * 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

interface DeepSeekBalanceCardProps {
  balance: DeepSeekBalance;
  /** 上次成功刷新时间（epoch 秒）；null 表示暂无 */
  fetchedAt: number | null;
  /** 低余额（低于阈值或不可用）：金额标红 */
  low: boolean;
}

/** DeepSeek 账号的余额卡：总余额 + 赠金/充值分项 + 币种 + 状态 + 更新时间（GOAL 拍板只此一张卡） */
export function DeepSeekBalanceCard({ balance, fetchedAt, low }: DeepSeekBalanceCardProps) {
  const { t } = useTranslation();
  return (
    <div className={`pcard balance-card${low ? " low" : ""}`}>
      <div className="usage-head">
        <span className="usage-title">{t("deepseek.title")}</span>
        <span className={`usage-pct${low ? " low-text" : ""}`}>
          {formatMoney(balance.currency, balance.total_balance)}
        </span>
      </div>
      <div className="balance-rows">
        <div className="balance-row">
          <span>{t("deepseek.granted")}</span>
          <span>{formatMoney(balance.currency, balance.granted_balance)}</span>
        </div>
        <div className="balance-row">
          <span>{t("deepseek.toppedUp")}</span>
          <span>{formatMoney(balance.currency, balance.topped_up_balance)}</span>
        </div>
        <div className="balance-row">
          <span>{t("deepseek.currency")}</span>
          <span>{balance.currency}</span>
        </div>
        <div className="balance-row">
          <span>{t("deepseek.status")}</span>
          <span>{balance.is_available ? t("deepseek.statusOk") : t("deepseek.statusUnavailable")}</span>
        </div>
        <div className="balance-row">
          <span>{t("deepseek.updatedAt")}</span>
          <span>{fetchedAt !== null ? formatTime(fetchedAt) : t("panel.noData")}</span>
        </div>
      </div>
    </div>
  );
}
