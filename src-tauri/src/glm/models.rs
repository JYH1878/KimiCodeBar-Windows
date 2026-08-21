//! GLM Coding Plan 额度接口（`GET /api/monitor/usage/quota/limit`）的 wire 模型与解析。
//!
//! 响应包络：`{"success":true,"code":200,"msg":"…","data":{…}}`——`success` 非 true
//! 或 `data` 缺失一律算解析失败（QuotaError::Parse）。`data.level` 为套餐档位字符串
//! （lite/pro/max），原样透出去 membership_level。
//!
//! `data.limits[]` 每行一个窗口，识别 `TOKENS_LIMIT`（旧套餐）与 `CREDIT_LIMIT`
//! （积分制新套餐，2026-08-21 实测）两种额度行：
//! - `unit:3, number:5` → 5 小时窗；`unit:6` → 周窗；
//! - unit/number 对不上的额度行不丢，按出现顺序兜底（第一个当 5 小时窗、
//!   第二个当周窗）——与参考实现 dsh-provider-balance 的 zaiAdapter 同规则；
//! - `CREDIT_LIMIT` 行带绝对量（`usage`=总量、`currentValue`=已用、`remaining`=剩余，
//!   单位积分），优先采用；其 5 小时窗行可能无 `nextResetTime`；
//! - `TIME_LIMIT`（工具月额度）本版不解析：不参与窗口认领。
//!
//! `percentage` = 已用百分比（0–100，可能小数），`nextResetTime` = epoch 毫秒。
//! 语义换算：本项目契约是「剩余」语义；无绝对量时 percent_remaining = 100 - percentage，
//! used/limit/remaining 以 100 为总量合成（下游历史采样、低额判定、5h 重置提醒
//! 全部按百分比口径工作）；有绝对量时按 剩余/总量 推算 percent_remaining。

use chrono::DateTime;
use serde::Deserialize;

use crate::quota::{KimiQuota, QuotaDetail, QuotaError};

/// 包络（code/msg 仅用于失败时的错误文案，不参与判定）
#[derive(Debug, Deserialize)]
pub struct QuotaResponseWire {
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub code: Option<i64>,
    #[serde(default)]
    pub msg: Option<String>,
    #[serde(default)]
    pub data: Option<QuotaDataWire>,
}

/// data 段：套餐档位 + 窗口行列表
#[derive(Debug, Deserialize)]
pub struct QuotaDataWire {
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub limits: Option<Vec<LimitRowWire>>,
}

/// 单行窗口。TIME_LIMIT 行整体不参与认领；其 usage/currentValue/remaining 字段与
/// CREDIT_LIMIT 同名，声明后 serde 照填，但只在绝对量分支读取
#[derive(Debug, Deserialize)]
pub struct LimitRowWire {
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub unit: Option<i64>,
    #[serde(default)]
    pub number: Option<i64>,
    #[serde(default)]
    pub percentage: Option<f64>,
    #[serde(rename = "nextResetTime", default)]
    pub next_reset_time: Option<i64>,
    /// CREDIT_LIMIT 行的总量（积分）
    #[serde(default)]
    pub usage: Option<f64>,
    /// CREDIT_LIMIT 行的已用（积分）
    #[serde(rename = "currentValue", default)]
    pub current_value: Option<f64>,
    /// CREDIT_LIMIT 行的剩余（积分）
    #[serde(default)]
    pub remaining: Option<f64>,
}

/// 解析额度响应 JSON 为领域模型（KimiQuota 契约：five_hour / weekly / membership_level）。
/// 包络失败（success 非 true / data 缺失）与 JSON 非法均为 QuotaError::Parse。
pub fn parse_quota(json: &str) -> Result<KimiQuota, QuotaError> {
    let resp: QuotaResponseWire =
        serde_json::from_str(json).map_err(|e| QuotaError::Parse(e.to_string()))?;
    if !resp.success {
        return Err(QuotaError::Parse(format!(
            "接口返回失败: code={}, msg={}",
            resp.code.map(|c| c.to_string()).unwrap_or_default(),
            resp.msg.as_deref().unwrap_or("n/a")
        )));
    }
    let data = resp
        .data
        .ok_or_else(|| QuotaError::Parse("响应缺少 data 段".to_string()))?;
    Ok(map_quota(&data))
}

/// data 段 → KimiQuota：先按 unit/number 精确认领窗口，认剩下的额度行
/// 按出现顺序兜底（第一个补 5 小时窗、第二个补周窗）；level 原样透出
fn map_quota(data: &QuotaDataWire) -> KimiQuota {
    let mut five_hour: Option<QuotaDetail> = None;
    let mut weekly: Option<QuotaDetail> = None;
    let mut fallback: Vec<QuotaDetail> = Vec::new();
    for row in data.limits.as_deref().unwrap_or(&[]) {
        // TOKENS_LIMIT（旧套餐）与 CREDIT_LIMIT（积分制新套餐）都认领；其余（TIME_LIMIT 等）跳过
        if !matches!(
            row.kind.as_deref(),
            Some("TOKENS_LIMIT") | Some("CREDIT_LIMIT")
        ) {
            continue;
        }
        let detail = make_window(row);
        if row.unit == Some(3) && row.number == Some(5) && five_hour.is_none() {
            five_hour = Some(detail);
        } else if row.unit == Some(6) && weekly.is_none() {
            weekly = Some(detail);
        } else {
            fallback.push(detail);
        }
    }
    if five_hour.is_none() && !fallback.is_empty() {
        five_hour = Some(fallback.remove(0));
    }
    if weekly.is_none() && !fallback.is_empty() {
        weekly = Some(fallback.remove(0));
    }
    KimiQuota {
        weekly,
        five_hour,
        total: None,
        membership_level: data.level.clone(),
        booster: None,
    }
}

/// 单行 → QuotaDetail：
/// - 带绝对量的行（CREDIT_LIMIT：usage=总量 / currentValue=已用 / remaining=剩余）
///   优先用绝对量，percent_remaining 按 剩余/总量 推算（钳 0–100）；
/// - 否则按百分比口径（TOKENS_LIMIT 无绝对量）：已用百分比钳 0–100，
///   以 100 为总量合成 used/limit/remaining；
///   nextResetTime（epoch 毫秒）非法/缺失容忍为 None
fn make_window(row: &LimitRowWire) -> QuotaDetail {
    let used_pct = row.percentage.unwrap_or(0.0).clamp(0.0, 100.0);
    let (used, limit, remaining, percent_remaining) =
        match (row.usage, row.current_value, row.remaining) {
            (Some(total), Some(used_abs), Some(remaining_abs)) if total > 0.0 => (
                used_abs,
                total,
                remaining_abs,
                (remaining_abs / total * 100.0).clamp(0.0, 100.0),
            ),
            _ => (used_pct, 100.0, 100.0 - used_pct, 100.0 - used_pct),
        };
    QuotaDetail {
        used,
        limit,
        remaining,
        reset_time: row
            .next_reset_time
            .and_then(DateTime::from_timestamp_millis),
        percent_remaining,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    /// 任务书标准样例（pro 档：5h 已用 42.5%、周窗已用 61%，外加一行 TIME_LIMIT）
    const FIXTURE: &str = r#"{"success":true,"code":200,"msg":"success","data":{"level":"pro","limits":[{"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":42.5,"nextResetTime":1784900000000},{"type":"TOKENS_LIMIT","unit":6,"number":1,"percentage":61.0,"nextResetTime":1785400000000},{"type":"TIME_LIMIT","usage":1000,"currentValue":120,"remaining":880}]}}"#;

    /// 积分制（CREDIT_LIMIT）真实响应样例：2026-08-21 实机诊断导出（lite 档新套餐）。
    /// 注意两个形态差异：type 是 CREDIT_LIMIT 而非 TOKENS_LIMIT；5h 行无 nextResetTime
    const CREDIT_FIXTURE: &str = r#"{"code":200,"data":{"level":"lite","limits":[{"currentValue":0,"number":5,"percentage":0,"remaining":2000,"type":"CREDIT_LIMIT","unit":3,"usage":2000},{"currentValue":0,"nextResetTime":1787926865998,"number":1,"percentage":0,"remaining":10000,"type":"CREDIT_LIMIT","unit":6,"usage":10000}]},"msg":"操作成功","success":true}"#;

    #[test]
    fn parses_standard_fixture() {
        let q = parse_quota(FIXTURE).unwrap();
        assert_eq!(q.membership_level.as_deref(), Some("pro"));

        let five_hour = q.five_hour.expect("5 小时窗应存在");
        assert!((five_hour.percent_remaining - 57.5).abs() < 1e-9);
        assert!((five_hour.used - 42.5).abs() < 1e-9);
        assert_eq!(five_hour.limit, 100.0);
        assert!((five_hour.remaining - 57.5).abs() < 1e-9);
        assert_eq!(
            five_hour.reset_time,
            DateTime::<Utc>::from_timestamp_millis(1784900000000)
        );

        let weekly = q.weekly.expect("周窗应存在");
        assert!((weekly.percent_remaining - 39.0).abs() < 1e-9);
        assert_eq!(
            weekly.reset_time,
            DateTime::<Utc>::from_timestamp_millis(1785400000000)
        );

        // GLM 无总额/月度/Booster 概念
        assert!(q.total.is_none());
        assert!(q.booster.is_none());
    }

    #[test]
    fn used_percent_converts_to_remaining() {
        // 已用转剩余换算：42.5 已用 → 57.5 剩余（反向验证的靶子：fixture 的
        // percentage 调大时本测试必须应变红，期望值不许两头写死）
        let q = parse_quota(FIXTURE).unwrap();
        let five_hour = q.five_hour.unwrap();
        assert!((five_hour.used + five_hour.percent_remaining - 100.0).abs() < 1e-9);
        assert!((five_hour.percent_remaining - (100.0 - 42.5)).abs() < 1e-9);
    }

    #[test]
    fn parses_credit_limit_fixture() {
        // 积分制新套餐：CREDIT_LIMIT 行同样被认领为 5h/周窗，且优先用绝对量
        let q = parse_quota(CREDIT_FIXTURE).unwrap();
        assert_eq!(q.membership_level.as_deref(), Some("lite"));

        let five_hour = q.five_hour.expect("5 小时窗应存在（CREDIT_LIMIT）");
        assert_eq!(five_hour.limit, 2000.0);
        assert_eq!(five_hour.used, 0.0);
        assert_eq!(five_hour.remaining, 2000.0);
        assert!((five_hour.percent_remaining - 100.0).abs() < 1e-9);
        assert!(
            five_hour.reset_time.is_none(),
            "CREDIT_LIMIT 5h 行无 nextResetTime，应容忍为 None"
        );

        let weekly = q.weekly.expect("周窗应存在（CREDIT_LIMIT）");
        assert_eq!(weekly.limit, 10000.0);
        assert_eq!(weekly.used, 0.0);
        assert_eq!(weekly.remaining, 10000.0);
        assert_eq!(
            weekly.reset_time,
            DateTime::<Utc>::from_timestamp_millis(1787926865998)
        );
    }

    #[test]
    fn credit_limit_partial_usage_maps_absolutes() {
        // 有消耗时：绝对量直达（已用 500/2000 → 剩余 1500，75%）
        let json = r#"{"success":true,"data":{"limits":[{"type":"CREDIT_LIMIT","unit":3,"number":5,"usage":2000,"currentValue":500,"remaining":1500,"percentage":25}]}}"#;
        let q = parse_quota(json).unwrap();
        let five_hour = q.five_hour.unwrap();
        assert_eq!(five_hour.used, 500.0);
        assert_eq!(five_hour.limit, 2000.0);
        assert_eq!(five_hour.remaining, 1500.0);
        assert!((five_hour.percent_remaining - 75.0).abs() < 1e-9);
    }

    #[test]
    fn credit_limit_without_absolutes_falls_back_to_percent() {
        // 防御：CREDIT_LIMIT 行缺绝对量字段时退回百分比合成（与 TOKENS_LIMIT 同路径）
        let json = r#"{"success":true,"data":{"limits":[{"type":"CREDIT_LIMIT","unit":3,"number":5,"percentage":40}]}}"#;
        let q = parse_quota(json).unwrap();
        let five_hour = q.five_hour.unwrap();
        assert!((five_hour.percent_remaining - 60.0).abs() < 1e-9);
        assert_eq!(five_hour.limit, 100.0);
    }

    #[test]
    fn mislabeled_windows_fall_back_by_order() {
        // unit/number 对不上的额度行不丢：第一个当 5 小时窗、第二个当周窗
        let json = r#"{"success":true,"data":{"level":"lite","limits":[{"type":"TOKENS_LIMIT","unit":99,"number":9,"percentage":10.0},{"type":"TOKENS_LIMIT","unit":88,"number":8,"percentage":20.0}]}}"#;
        let q = parse_quota(json).unwrap();
        assert!((q.five_hour.unwrap().percent_remaining - 90.0).abs() < 1e-9);
        assert!((q.weekly.unwrap().percent_remaining - 80.0).abs() < 1e-9);
    }

    #[test]
    fn precise_claim_wins_over_fallback_order() {
        // 周窗（unit:6）排在 5 小时窗前面：精确认领优先，兜底只补未认领的窗口
        let json = r#"{"success":true,"data":{"limits":[{"type":"TOKENS_LIMIT","unit":6,"number":1,"percentage":61.0},{"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":42.5}]}}"#;
        let q = parse_quota(json).unwrap();
        assert!((q.five_hour.unwrap().percent_remaining - 57.5).abs() < 1e-9);
        assert!((q.weekly.unwrap().percent_remaining - 39.0).abs() < 1e-9);
    }

    #[test]
    fn fallback_only_fills_unclaimed_windows() {
        // 5 小时窗已精确认领，错位行兜底补周窗（不顶掉已认领的 5 小时窗）
        let json = r#"{"success":true,"data":{"limits":[{"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":42.5},{"type":"TOKENS_LIMIT","unit":99,"number":9,"percentage":70.0}]}}"#;
        let q = parse_quota(json).unwrap();
        assert!((q.five_hour.unwrap().percent_remaining - 57.5).abs() < 1e-9);
        assert!((q.weekly.unwrap().percent_remaining - 30.0).abs() < 1e-9);
    }

    #[test]
    fn success_false_is_parse_error() {
        // 无 Coding Plan 订阅时服务端的真实形态（2026-08-21 实测：code=500, success=false）
        let json = r#"{"success":false,"code":401,"msg":"invalid token","data":null}"#;
        let err = parse_quota(json).unwrap_err();
        match err {
            QuotaError::Parse(msg) => {
                assert!(msg.contains("401"), "错误文案应带 code: {msg}");
                assert!(msg.contains("invalid token"), "错误文案应带 msg: {msg}");
            }
            other => panic!("应为 Parse 错误，实际: {other}"),
        }
    }

    #[test]
    fn missing_data_is_parse_error() {
        assert!(matches!(
            parse_quota(r#"{"success":true,"code":200,"msg":"ok"}"#),
            Err(QuotaError::Parse(_))
        ));
    }

    #[test]
    fn missing_limits_yields_empty_windows() {
        let q = parse_quota(r#"{"success":true,"data":{"level":"max"}}"#).unwrap();
        assert!(q.five_hour.is_none());
        assert!(q.weekly.is_none());
        assert_eq!(q.membership_level.as_deref(), Some("max"));
    }

    #[test]
    fn missing_level_is_none() {
        let q = parse_quota(r#"{"success":true,"data":{"limits":[]}}"#).unwrap();
        assert!(q.membership_level.is_none());
    }

    #[test]
    fn percentage_zero_means_full_remaining() {
        let json = r#"{"success":true,"data":{"limits":[{"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":0}]}}"#;
        let q = parse_quota(json).unwrap();
        assert_eq!(q.five_hour.unwrap().percent_remaining, 100.0);
    }

    #[test]
    fn percentage_hundred_means_empty() {
        let json = r#"{"success":true,"data":{"limits":[{"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":100}]}}"#;
        let q = parse_quota(json).unwrap();
        let five_hour = q.five_hour.unwrap();
        assert_eq!(five_hour.percent_remaining, 0.0);
        assert_eq!(five_hour.remaining, 0.0);
    }

    #[test]
    fn percentage_out_of_range_clamped() {
        // 防御：接口异常返回 >100 / 负数时钳回 0–100（剩余语义不溢出）
        let json = r#"{"success":true,"data":{"limits":[{"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":150},{"type":"TOKENS_LIMIT","unit":6,"number":1,"percentage":-10}]}}"#;
        let q = parse_quota(json).unwrap();
        assert_eq!(q.five_hour.unwrap().percent_remaining, 0.0);
        assert_eq!(q.weekly.unwrap().percent_remaining, 100.0);
    }

    #[test]
    fn missing_next_reset_time_is_none() {
        let json = r#"{"success":true,"data":{"limits":[{"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":42.5}]}}"#;
        let q = parse_quota(json).unwrap();
        assert!(q.five_hour.unwrap().reset_time.is_none());
    }

    #[test]
    fn time_limit_row_ignored() {
        // TIME_LIMIT（工具月额度）本版不解析：不参与窗口认领，也不报错
        let json = r#"{"success":true,"data":{"limits":[{"type":"TIME_LIMIT","usage":1000,"currentValue":120,"remaining":880}]}}"#;
        let q = parse_quota(json).unwrap();
        assert!(q.five_hour.is_none());
        assert!(q.weekly.is_none());
    }

    #[test]
    fn unknown_fields_tolerated() {
        // 接口加新字段（含 limits 行内新字段）不应挂：serde 默认容忍未知字段
        let json = r#"{"success":true,"code":200,"msg":"ok","extra":"x","data":{"level":"pro","extra2":1,"limits":[{"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":42.5,"nextResetTime":1784900000000,"usageDetails":[{"modelCode":"glm-4.6","usage":1}]}]}}"#;
        let q = parse_quota(json).unwrap();
        assert!((q.five_hour.unwrap().percent_remaining - 57.5).abs() < 1e-9);
    }

    #[test]
    fn invalid_json_is_parse_error() {
        assert!(matches!(parse_quota("not json"), Err(QuotaError::Parse(_))));
    }

    #[test]
    fn parsed_quota_feeds_low_warning() {
        // 与下游低额判定闭环：fixture 剩余 57.5%/39% 远高于阈值 20% → 不低额；
        // 已用 85% → 剩余 15% < 20% → 低额（同一条换算链路的端到端断言）
        let q = parse_quota(FIXTURE).unwrap();
        assert!(!crate::quota::needs_low_warning(&q, 20.0));
        let low = parse_quota(r#"{"success":true,"data":{"limits":[{"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":85.0}]}}"#).unwrap();
        assert!(crate::quota::needs_low_warning(&low, 20.0));
    }

    #[test]
    fn parsed_quota_feeds_history_sampling() {
        // 与历史采样闭环：已用 42.5% → 采样点 five_hour = 42.5（已用口径）
        let q = parse_quota(FIXTURE).unwrap();
        let p = crate::history::sample_point(&q, None, 1234);
        assert!((p.five_hour.unwrap() - 42.5).abs() < 1e-9);
        assert!((p.weekly.unwrap() - 61.0).abs() < 1e-9);
        assert!(p.monthly.is_none());
    }
}
