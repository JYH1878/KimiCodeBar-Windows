//! `/coding/v1/usages` 响应的 wire 模型（serde 直连 JSON）。
//!
//! 坑点（对齐 Mac 版实现）：
//! - 用量数值字段在 JSON 里是**字符串**
//! - proto3 JSON：`false` 的布尔字段会被省略，必须 `Option`/`default` 处理
//! - `boosterWallet.balance.amountLeft` 单位是 1e-8 元
//!
//! 字段定义以 `KimiCodeBar-Mac/Windows/src/KimiCodeBar.Core/Services/QuotaParser.cs`
//! 和 `KimiCodeBar-Mac/Windows/docs/system_design.md` 中的 JSON schema 为准。

use serde::Deserialize;

/// GET /coding/v1/usages 的完整响应
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageResponse {
    /// 7 天（周）用量窗口
    pub usage: Option<UsageDetailWire>,
    /// 细分时间窗口列表；其中 `window.duration == 300`（分钟）的是 5 小时窗口
    pub limits: Option<Vec<LimitsEntry>>,
    /// 总额度（无 resetTime）
    pub total_quota: Option<TotalQuotaWire>,
    pub user: Option<UserWire>,
    /// 加油包钱包；未开通时整个字段缺失
    pub booster_wallet: Option<BoosterWalletWire>,
}

/// 单段用量明细的 wire 形态（顶层 `usage` 与 `limits[].detail` 同构，共用）。
/// 数值字段在 JSON 里是字符串形式的整数。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDetailWire {
    pub limit: Option<String>,
    pub used: Option<String>,
    pub remaining: Option<String>,
    /// ISO8601，含毫秒；解析失败容忍为 None
    pub reset_time: Option<String>,
}

/// `limits[]` 中的一项：窗口描述 + 该窗口的用量明细
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitsEntry {
    pub window: Option<Window>,
    pub detail: Option<UsageDetailWire>,
}

/// 时间窗口描述；`duration` 单位分钟，300 即 5 小时窗口
#[derive(Debug, Clone, Deserialize)]
pub struct Window {
    pub duration: Option<i64>,
}

/// 总额度的 wire 形态（只有 limit / remaining，无 used / resetTime）
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TotalQuotaWire {
    pub limit: Option<String>,
    pub remaining: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserWire {
    pub membership: Option<MembershipWire>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MembershipWire {
    /// 会员等级（如 LEVEL_FREE）
    pub level: Option<String>,
}

/// 加油包钱包的 wire 形态
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoosterWalletWire {
    /// 状态字符串（STATUS_ACTIVE / STATUS_ENABLED 视为启用）
    pub status: Option<String>,
    pub balance: Option<Balance>,
    /// proto3 JSON 中 false 会被省略，缺省即"未启用月度上限"
    pub monthly_charge_limit_enabled: Option<bool>,
    pub monthly_charge_limit: Option<Money>,
    pub monthly_used: Option<Money>,
    pub topup_limit: Option<Money>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Balance {
    pub amount: Option<String>,
    /// 真实余额，单位 1e-8 元（如 315250700 = ¥3.15）
    pub amount_left: Option<String>,
    pub unit: Option<String>,
}

/// 金额（priceInCents 为字符串形式的分）
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Money {
    pub currency: Option<String>,
    pub price_in_cents: Option<String>,
}
