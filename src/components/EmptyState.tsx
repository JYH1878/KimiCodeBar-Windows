interface EmptyStateProps {
  /** 点击"打开设置"时触发 */
  onOpenSettings: () => void;
}

/** 未配置凭证时的引导页 */
export function EmptyState({ onOpenSettings }: EmptyStateProps) {
  return (
    <div className="pcard empty-state">
      <p className="muted-text">尚未配置凭证</p>
      <button className="btn primary" onClick={onOpenSettings}>
        打开设置
      </button>
    </div>
  );
}
