import { useTranslation } from "react-i18next";

interface EmptyStateProps {
  /** 点击"打开设置"时触发 */
  onOpenSettings: () => void;
}

/** 未配置凭证时的引导页 */
export function EmptyState({ onOpenSettings }: EmptyStateProps) {
  const { t } = useTranslation();
  return (
    <div className="pcard empty-state">
      <p className="muted-text">{t("empty.noCredential")}</p>
      <button className="btn primary" onClick={onOpenSettings}>
        {t("empty.openSettings")}
      </button>
    </div>
  );
}
