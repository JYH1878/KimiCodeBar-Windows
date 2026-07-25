//! 配额领域模型与解析：wire JSON → 统一"剩余%"语义的领域对象。
//!
//! 注意：Mac 版显示"已用%"，本应用按需求统一为**剩余百分比**。

use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::kimi::models::{BoosterWalletWire, Money, UsageResponse};

#[derive(Debug, Error)]
pub enum QuotaError {
    #[error("网络错误: {0}")]
    Http(String),
    #[error("API 错误 (HTTP {status}): {message}")]
    Api { status: u16, message: String },
    #[error("凭证无效或已过期 (HTTP 401/403)")]
    Unauthorized,
    #[error("响应解析失败: {0}")]
    Parse(String),
}

/// 单个时间窗口（7 天 / 5 小时）的用量
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QuotaDetail {
    pub used: f64,
    pub limit: f64,
    pub remaining: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset_time: Option<DateTime<Utc>>,
    /// 剩余百分比 0–100
    pub percent_remaining: f64,
}

impl QuotaDetail {
    /// 距离重置还有多久，格式如 "3天2小时后重置" / "即将重置"。
    /// （移植 C# QuotaDetail.TimeUntilReset：取天/小时/分钟组件逐级降档）
    pub fn time_until_reset(&self) -> String {
        let Some(reset) = self.reset_time else {
            return "未知".to_string();
        };
        let now = Utc::now();
        if reset <= now {
            return "即将重置".to_string();
        }

        let span = reset - now;
        let days = span.num_days();
        let hours = span.num_hours() % 24;
        let minutes = span.num_minutes() % 60;

        if days > 0 {
            return format!("{}天{}小时后重置", days, hours);
        }
        if hours > 0 {
            return format!("{}小时{}分钟后重置", hours, minutes);
        }
        if minutes > 0 {
            return format!("{}分钟后重置", minutes);
        }
        "即将重置".to_string()
    }

    /// 重置时间的展示文本，格式 MM-dd HH:mm（本地时区，对齐 Mac 版 DateFormatter 行为）。
    pub fn reset_time_text(&self) -> String {
        match self.reset_time {
            Some(t) => t.with_timezone(&Local).format("%m-%d %H:%M").to_string(),
            None => "未知".to_string(),
        }
    }
}

/// 总额度（无 resetTime）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TotalQuotaInfo {
    pub limit: f64,
    pub remaining: f64,
    pub percent_remaining: f64,
}

/// Booster 钱包（金额已换算为元）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BoosterInfo {
    pub enabled: bool,
    pub balance_yuan: f64,
    pub monthly_charge_limit_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monthly_charge_limit_yuan: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monthly_used_yuan: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topup_limit_yuan: Option<f64>,
}

/// 解析后的完整配额
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KimiQuota {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weekly: Option<QuotaDetail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub five_hour: Option<QuotaDetail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<TotalQuotaInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub membership_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub booster: Option<BoosterInfo>,
}

/// 解析 `/coding/v1/usages` 响应 JSON 为领域模型。
///
/// 移植 C# `QuotaParser.Parse`：5 小时窗口取 `limits[]` 中 `window.duration == 300`
/// 的项，顶层 `usage` 是 7 天窗口；与 C# 的差异：
/// - 百分比统一为**剩余语义**（remaining / limit * 100，Mac 版是已用%）；
/// - 各段缺失时对应字段为 `None`（C# 是零值对象）；
/// - JSON 非法时返回 `QuotaError::Parse`（C# 返回空结构）。
pub fn parse_usage(json: &str) -> Result<KimiQuota, QuotaError> {
    let resp: UsageResponse =
        serde_json::from_str(json).map_err(|e| QuotaError::Parse(e.to_string()))?;

    // 7 天窗口：顶层 usage 段
    let weekly = resp.usage.as_ref().map(|u| {
        make_detail(
            u.limit.as_deref(),
            u.used.as_deref(),
            u.remaining.as_deref(),
            u.reset_time.as_deref(),
        )
    });

    // 5 小时窗口：limits[] 中 window.duration == 300 的项；
    // 项存在即产出（detail 缺失按零值明细，与 C# 一致），项不存在为 None
    let five_hour = resp
        .limits
        .as_ref()
        .and_then(|ls| {
            ls.iter()
                .find(|l| l.window.as_ref().and_then(|w| w.duration) == Some(300))
        })
        .map(|entry| {
            let d = entry.detail.as_ref();
            make_detail(
                d.and_then(|x| x.limit.as_deref()),
                d.and_then(|x| x.used.as_deref()),
                d.and_then(|x| x.remaining.as_deref()),
                d.and_then(|x| x.reset_time.as_deref()),
            )
        });

    // 总额度：无 resetTime，used 由 remaining 反推（与 C# 一致）；
    // totalQuota:{}（limit/remaining 均缺失）视为无总额度，产出 None
    let total = resp.total_quota.as_ref().and_then(|t| {
        if t.limit.is_none() && t.remaining.is_none() {
            return None;
        }
        let d = make_detail(t.limit.as_deref(), None, t.remaining.as_deref(), None);
        Some(TotalQuotaInfo {
            limit: d.limit,
            remaining: d.remaining,
            percent_remaining: d.percent_remaining,
        })
    });

    let membership_level = resp
        .user
        .as_ref()
        .and_then(|u| u.membership.as_ref())
        .and_then(|m| m.level.clone());

    let booster = resp.booster_wallet.as_ref().map(make_booster);

    Ok(KimiQuota {
        weekly,
        five_hour,
        total,
        membership_level,
        booster,
    })
}

/// 任一已存在配额项（weekly / five_hour / total）的剩余百分比低于阈值
/// （如 20.0）时返回 true，用于托盘变红告警。缺失的段不参与判定。
pub fn needs_low_warning(quota: &KimiQuota, threshold_pct: f64) -> bool {
    quota
        .weekly
        .as_ref()
        .is_some_and(|w| w.percent_remaining < threshold_pct)
        || quota
            .five_hour
            .as_ref()
            .is_some_and(|f| f.percent_remaining < threshold_pct)
        || quota
            .total
            .as_ref()
            .is_some_and(|t| t.percent_remaining < threshold_pct)
}

/// 构造单条用量明细（移植 C# `MakeDetail`，百分比改为剩余语义）。
fn make_detail(
    limit: Option<&str>,
    used: Option<&str>,
    remaining: Option<&str>,
    reset_time: Option<&str>,
) -> QuotaDetail {
    let li = parse_num(limit).unwrap_or(0.0);

    // used 缺失（或非法）时用 limit - remaining 反推；两者都缺则按 0 已用
    let us = match parse_num(used) {
        Some(v) => v,
        None => match parse_num(remaining) {
            Some(re) => (li - re).max(0.0),
            None => 0.0,
        },
    };
    // remaining 一律由 limit - used 反推（与 C# 一致，不信任两端同时给出的不一致数据）
    let re = (li - us).max(0.0);
    // 剩余百分比（本应用统一剩余语义；Mac 版是已用%）
    let pct = if li > 0.0 { re / li * 100.0 } else { 0.0 };

    QuotaDetail {
        used: us,
        limit: li,
        remaining: re,
        reset_time: parse_reset_time(reset_time),
        percent_remaining: pct,
    }
}

/// 构造加油包钱包（处理余额 1e-8 换算与 proto3 缺省布尔）。
fn make_booster(raw: &BoosterWalletWire) -> BoosterInfo {
    let status = raw.status.as_deref().unwrap_or("STATUS_UNKNOWN");
    let upper = status.to_uppercase();
    let enabled = upper == "STATUS_ACTIVE" || upper == "STATUS_ENABLED";

    // 真实余额：仅当加油包启用且接口返回 amountLeft（单位 1e-8 元）时读取；
    // 未启用时接口可能返回"月度上限 - 月度消费"相关值（如 ¥75）而非真实余额，显示 ¥0。
    let balance_yuan = if enabled {
        raw.balance
            .as_ref()
            .and_then(|b| b.amount_left.as_deref())
            .and_then(|s| s.trim().parse::<f64>().ok())
            .map(|v| (v / 100_000_000.0).max(0.0))
            .unwrap_or(0.0)
    } else {
        0.0
    };

    BoosterInfo {
        enabled,
        balance_yuan,
        // proto3 JSON 中 false 会被省略，缺省即"未启用月度上限"
        monthly_charge_limit_enabled: raw.monthly_charge_limit_enabled.unwrap_or(false),
        monthly_charge_limit_yuan: money_yuan(raw.monthly_charge_limit.as_ref()),
        monthly_used_yuan: money_yuan(raw.monthly_used.as_ref()),
        topup_limit_yuan: money_yuan(raw.topup_limit.as_ref()),
    }
}

/// priceInCents（字符串形式的分）→ 元；字段缺失或解析失败为 None。
fn money_yuan(m: Option<&Money>) -> Option<f64> {
    m.and_then(|m| m.price_in_cents.as_deref())
        .and_then(|s| s.trim().parse::<i64>().ok())
        .map(|c| c as f64 / 100.0)
}

/// 解析含毫秒的 ISO8601 resetTime；失败容忍为 None。
fn parse_reset_time(s: Option<&str>) -> Option<DateTime<Utc>> {
    let s = s?;
    if s.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .ok()
}

/// 解析数值字符串（接口返回字符串形式的整数），非法按 None 处理（对齐 C# TryParse 落空分支）。
fn parse_num(s: Option<&str>) -> Option<f64> {
    s.and_then(|v| v.trim().parse::<i64>().ok())
        .map(|v| v as f64)
}
