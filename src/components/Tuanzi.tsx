import { useTranslation } from "react-i18next";

/** 蓝团子（Kimi Code 吉祥物）：按上游 macOS 原版 AnimatedKimiCodeLogo 的参数化矢量规格复刻
    （32×22 视窗：身体圆角矩形 30×20 r6 + 双眼胶囊 2.8×8，Kimi 蓝 #3B82F5），
    眨眼（3s 周期）与左右看（12s 周期）由 CSS 驱动，矢量任意缩放无锯齿、眼睛随主题变色 */
export function Tuanzi({ className }: { className?: string }) {
  const { t } = useTranslation();
  return (
    <svg className={className} viewBox="0 0 32 22" role="img" aria-label={t("panel.mascotTitle")}>
      <title>{t("panel.mascotTitle")}</title>
      <rect className="tuanzi-body" x="1" y="1" width="30" height="20" rx="6" />
      <g className="tuanzi-eyes">
        <rect className="tuanzi-eye" x="11.8" y="7" width="2.8" height="8" rx="1.4" />
        <rect className="tuanzi-eye" x="17.4" y="7" width="2.8" height="8" rx="1.4" />
      </g>
    </svg>
  );
}
