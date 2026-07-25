//! 用量解析边界测试，移植自
//! `KimiCodeBar-Mac/Windows/tests/KimiCodeBar.Core.Tests/QuotaParserTests.cs`，
//! 覆盖 Mac 已知行为：余额 1e-8、proto3 缺省布尔、5H 取 duration==300、
//! 数值缺失反推、重置时间解析与文案。
//! 关键差异：本应用 `percent_remaining` 是**剩余语义**（Mac 版 Percentage 是已用%）。

use chrono::{DateTime, Duration, Utc};
use kimicodebar::quota::{needs_low_warning, parse_usage, KimiQuota, QuotaDetail, QuotaError};

/// 读取 tests/fixtures/ 下的 JSON 样本
fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读取 fixture 失败 {}: {}", path, e))
}

/// 断言文本符合固定格式 MM-dd HH:mm（只看形状，不依赖具体时区）
fn assert_mmdd_hhmm(s: &str) {
    let chars: Vec<char> = s.chars().collect();
    assert_eq!(chars.len(), 11, "格式应为 MM-dd HH:mm，实际: {s}");
    for (i, c) in chars.iter().enumerate() {
        match i {
            2 => assert_eq!(*c, '-', "位置 {i} 应为 '-'，实际: {s}"),
            5 => assert_eq!(*c, ' ', "位置 {i} 应为空格，实际: {s}"),
            8 => assert_eq!(*c, ':', "位置 {i} 应为 ':'，实际: {s}"),
            _ => assert!(c.is_ascii_digit(), "位置 {i} 应为数字，实际: {s}"),
        }
    }
}

#[test]
fn weekly_percent_is_remaining_semantics() {
    let q = parse_usage(&fixture("full_response.json")).unwrap();
    let w = q.weekly.as_ref().expect("weekly 应存在");
    assert_eq!(w.limit, 100.0);
    assert_eq!(w.used, 30.0);
    assert_eq!(w.remaining, 70.0);
    // 剩余语义：remaining/limit*100 = 70（Mac 版是已用 30）
    assert!((w.percent_remaining - 70.0).abs() < 1e-9);
}

#[test]
fn five_hour_selected_by_duration_300() {
    let q = parse_usage(&fixture("full_response.json")).unwrap();
    let f = q.five_hour.as_ref().expect("five_hour 应存在");
    assert_eq!(f.limit, 50.0);
    assert_eq!(f.used, 10.0);
    assert_eq!(f.remaining, 40.0);
    // 剩余语义：40/50*100 = 80（Mac 版是已用 20）
    assert!((f.percent_remaining - 80.0).abs() < 1e-9);
}

#[test]
fn five_hour_missing_duration_300_is_none() {
    let q = parse_usage(&fixture("proto3_omitted.json")).unwrap();
    assert!(q.five_hour.is_none());
}

#[test]
fn balance_yuan_uses_amount_left_divided_by_1e8() {
    let q = parse_usage(&fixture("full_response.json")).unwrap();
    let b = q.booster.as_ref().expect("booster 应存在");
    // 315250700 / 1e8 = 3.152507
    assert!((b.balance_yuan - 3.152507).abs() < 1e-9);
    assert!(b.enabled);
}

#[test]
fn proto3_omitted_bool_defaults_monthly_limit_disabled_false() {
    let q = parse_usage(&fixture("full_response.json")).unwrap();
    let b = q.booster.as_ref().expect("booster 应存在");
    // 样本未包含 monthlyChargeLimitEnabled，应缺省为 false
    assert!(!b.monthly_charge_limit_enabled);
    // 分 → 元
    assert_eq!(b.monthly_charge_limit_yuan, Some(99.0));
    assert_eq!(b.monthly_used_yuan, Some(12.0));
    assert_eq!(b.topup_limit_yuan, Some(500.0));
}

#[test]
fn disabled_wallet_balance_is_zero_even_if_amount_left_present() {
    let json = r#"{
      "boosterWallet": {
        "status": "STATUS_DISABLED",
        "balance": { "amountLeft": "7500000000" },
        "monthlyChargeLimitEnabled": true,
        "monthlyChargeLimit": { "currency": "CNY", "priceInCents": "9900" }
      }
    }"#;
    let q = parse_usage(json).unwrap();
    let b = q.booster.as_ref().expect("booster 应存在");
    assert!(!b.enabled);
    // 未启用时真实余额显示 ¥0（接口可能返回月度上限相关值，需忽略）
    assert_eq!(b.balance_yuan, 0.0);
    // 但显式 true 的开关仍应被尊重
    assert!(b.monthly_charge_limit_enabled);
    assert_eq!(b.monthly_charge_limit_yuan, Some(99.0));
}

#[test]
fn status_enabled_alias_treated_as_enabled() {
    let q = parse_usage(r#"{ "boosterWallet": { "status": "STATUS_ENABLED" } }"#).unwrap();
    assert!(q.booster.unwrap().enabled);
    // 状态比较大小写不敏感
    let q = parse_usage(r#"{ "boosterWallet": { "status": "status_active" } }"#).unwrap();
    assert!(q.booster.unwrap().enabled);
}

#[test]
fn reset_time_parsed_and_formatted() {
    let q = parse_usage(&fixture("full_response.json")).unwrap();
    let w = q.weekly.as_ref().expect("weekly 应存在");
    // 含毫秒的 ISO8601 正确解析（+08:00 → UTC 04:00）
    let expected = DateTime::parse_from_rfc3339("2027-01-10T12:00:00.000+08:00")
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(w.reset_time, Some(expected));
    // 重置时间文本固定格式 MM-dd HH:mm
    assert_mmdd_hhmm(&w.reset_time_text());
    // 未来时刻的倒计时文案形如 "…后重置"
    assert!(w.time_until_reset().ends_with("后重置"));
}

#[test]
fn invalid_reset_time_tolerated_as_none() {
    let q = parse_usage(r#"{"usage":{"limit":"100","used":"30","resetTime":"not-a-date"}}"#).unwrap();
    let w = q.weekly.expect("weekly 应存在");
    assert!(w.reset_time.is_none());
    assert_eq!(w.reset_time_text(), "未知");
    assert_eq!(w.time_until_reset(), "未知");
}

#[test]
fn invalid_json_returns_parse_error() {
    let err = parse_usage("not json at all").unwrap_err();
    assert!(matches!(err, QuotaError::Parse(_)));
}

#[test]
fn empty_json_object_yields_all_none() {
    let q = parse_usage("{}").unwrap();
    assert!(q.weekly.is_none());
    assert!(q.five_hour.is_none());
    assert!(q.total.is_none());
    assert!(q.booster.is_none());
    assert!(q.membership_level.is_none());
}

#[test]
fn missing_values_backfilled_from_each_other() {
    let q = parse_usage(&fixture("missing_values.json")).unwrap();
    // used 缺失 → 用 limit - remaining 反推
    let w = q.weekly.as_ref().expect("weekly 应存在");
    assert_eq!(w.used, 30.0);
    assert_eq!(w.remaining, 70.0);
    assert!((w.percent_remaining - 70.0).abs() < 1e-9);
    // remaining 缺失 → 用 limit - used 反推
    let f = q.five_hour.as_ref().expect("five_hour 应存在");
    assert_eq!(f.used, 10.0);
    assert_eq!(f.remaining, 40.0);
    assert!((f.percent_remaining - 80.0).abs() < 1e-9);
}

#[test]
fn proto3_omitted_sections_default_gracefully() {
    let q = parse_usage(&fixture("proto3_omitted.json")).unwrap();
    // usage 仅有 limit：used/remaining 均缺失 → 按未使用处理
    let w = q.weekly.as_ref().expect("weekly 应存在");
    assert_eq!(w.used, 0.0);
    assert_eq!(w.remaining, 200.0);
    assert!((w.percent_remaining - 100.0).abs() < 1e-9);
    // membership.level 缺失 → None
    assert!(q.membership_level.is_none());
    // boosterWallet 只有 status：启用、余额 ¥0、开关缺省 false、金额字段 None
    let b = q.booster.as_ref().expect("booster 应存在");
    assert!(b.enabled);
    assert_eq!(b.balance_yuan, 0.0);
    assert!(!b.monthly_charge_limit_enabled);
    assert_eq!(b.monthly_charge_limit_yuan, None);
    assert_eq!(b.monthly_used_yuan, None);
    assert_eq!(b.topup_limit_yuan, None);
    // totalQuota 未使用 → 剩余 100%
    let t = q.total.as_ref().expect("total 应存在");
    assert!((t.percent_remaining - 100.0).abs() < 1e-9);
}

#[test]
fn no_booster_wallet_yields_none() {
    let q = parse_usage(&fixture("no_booster_wallet.json")).unwrap();
    assert!(q.booster.is_none());
    assert_eq!(q.membership_level.as_deref(), Some("LEVEL_FREE"));
    let t = q.total.as_ref().expect("total 应存在");
    assert_eq!(t.limit, 500.0);
    assert_eq!(t.remaining, 300.0);
    assert!((t.percent_remaining - 60.0).abs() < 1e-9);
}

#[test]
fn time_until_reset_text_variants() {
    let now = Utc::now();
    let at = |d: Duration| QuotaDetail {
        reset_time: Some(now + d),
        ..Default::default()
    };
    // 各档位预留 ~45s 余量，避免构造与断言之间跨分钟边界导致抖动
    assert_eq!(
        at(Duration::days(3) + Duration::hours(2) + Duration::minutes(30) + Duration::seconds(45))
            .time_until_reset(),
        "3天2小时后重置"
    );
    assert_eq!(
        at(Duration::hours(5) + Duration::minutes(30) + Duration::seconds(45)).time_until_reset(),
        "5小时30分钟后重置"
    );
    assert_eq!(
        at(Duration::minutes(45) + Duration::seconds(45)).time_until_reset(),
        "45分钟后重置"
    );
    // 不足 1 分钟与已过时间都显示"即将重置"
    assert_eq!(at(Duration::seconds(30)).time_until_reset(), "即将重置");
    assert_eq!(at(-Duration::hours(1)).time_until_reset(), "即将重置");
    // 无 resetTime
    assert_eq!(QuotaDetail::default().time_until_reset(), "未知");
}

#[test]
fn low_warning_not_triggered_when_all_windows_healthy() {
    let q = parse_usage(&fixture("full_response.json")).unwrap();
    // 最低的是 total 60%（weekly 70 / five_hour 80）
    assert!(!needs_low_warning(&q, 20.0));
}

#[test]
fn low_warning_triggered_when_any_window_below_threshold() {
    let q = parse_usage(&fixture("low_quota.json")).unwrap();
    // weekly 剩 5%、five_hour 剩 10%、total 剩 98%
    assert!(needs_low_warning(&q, 20.0));
    // 阈值压到不超过 5% 则不告警（判定是严格小于）
    assert!(!needs_low_warning(&q, 5.0));
    assert!(!needs_low_warning(&q, 3.0));
}

#[test]
fn low_warning_threshold_is_strict_less_than() {
    let q = parse_usage(r#"{"usage":{"limit":"100","used":"80","remaining":"20"}}"#).unwrap();
    // 恰好等于阈值不告警
    assert!(!needs_low_warning(&q, 20.0));
    assert!(needs_low_warning(&q, 20.1));
}

#[test]
fn low_warning_also_covers_total_quota() {
    let q = parse_usage(r#"{"totalQuota":{"limit":"500","remaining":"40"}}"#).unwrap();
    // total 剩 8%
    assert!(needs_low_warning(&q, 20.0));
}

#[test]
fn low_warning_empty_quota_never_triggers() {
    assert!(!needs_low_warning(&KimiQuota::default(), 20.0));
}

/// 真实脱敏响应（tests/fixtures/usages_real_sanitized.json）：
/// 验证线上实际返回形状（含未知字段 parallel/authentication/domain/subType）
/// 能被容忍解析，且各段取值符合"剩余%"语义。
#[test]
fn real_sanitized_response_parses_correctly() {
    let q = parse_usage(&fixture("usages_real_sanitized.json")).unwrap();

    // totalQuota:{}（limit/remaining 均缺失）→ total 为 None
    assert!(q.total.is_none());

    // weekly：limit=100 used=13 → 剩 87%
    let w = q.weekly.as_ref().expect("weekly 应存在");
    assert!((w.percent_remaining - 87.0).abs() < 1e-9);

    // five_hour（limits[] 中 window.duration==300）：limit=100 used=64 → 剩 36%
    let f = q.five_hour.as_ref().expect("five_hour 应存在");
    assert!((f.percent_remaining - 36.0).abs() < 1e-9);

    // membership.level 原样透传
    assert_eq!(q.membership_level.as_deref(), Some("LEVEL_INTERMEDIATE"));

    // boosterWallet.status=STATUS_DISABLED → 未启用
    let b = q.booster.as_ref().expect("booster 应存在");
    assert!(!b.enabled);

    // 未知字段（parallel/authentication/domain 等）被忽略：
    // 能走到这里即证明 serde 未因多余字段报错
}
