/** 会员等级枚举 → 官方档位名（音乐速度记号，由慢到快对应由低到高）；未知等级原样显示 */
const LEVEL_NAMES: Record<string, string> = {
  LEVEL_FREE: "Andante",
  LEVEL_BASIC: "Moderato",
  LEVEL_INTERMEDIATE: "Allegretto",
  LEVEL_ADVANCED: "Allegro",
};

interface MembershipCardProps {
  /** LEVEL_FREE / LEVEL_BASIC / LEVEL_INTERMEDIATE / LEVEL_ADVANCED，可能缺失 */
  level?: string;
}

/** 会员等级小卡 */
export function MembershipCard({ level }: MembershipCardProps) {
  const name = level ? (LEVEL_NAMES[level] ?? level) : "未知";
  return (
    <div className="pcard mini-card">
      <div className="mini-title">会员等级</div>
      <div className="mini-value">{name}</div>
    </div>
  );
}
