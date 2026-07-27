import { useTranslation } from "react-i18next";

interface ErrorBannerProps {
  /** 最近一次错误信息 */
  error: string;
  /** 点击"重试"时触发（通常是 refreshNow） */
  onRetry: () => void;
}

/** 非阻断错误横幅：展示错误但不遮挡已有缓存数据 */
export function ErrorBanner({ error, onRetry }: ErrorBannerProps) {
  const { t } = useTranslation();
  return (
    <div className="error-banner">
      <span className="error-text">{error}</span>
      <button className="btn" onClick={onRetry}>
        {t("error.retry")}
      </button>
    </div>
  );
}
