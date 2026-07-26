//! 网页端月度总量解析的集成测试：真实响应样例（token-monitor 同款形状）、
//! snake_case 别名变体。其余防御用例（data 包裹、ratio>1、FEATURE_OTHER 等）
//! 见 kimi::web 模块内单测。

use kimicodebar::kimi::web::{parse_subscription_stats, MonthlyInfo, WebError};

/// 读取 tests/fixtures/ 下的 JSON 样本
fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读取 fixture 失败 {}: {}", path, e))
}

#[test]
fn sample_response_total_code_kimi_split() {
    let m = parse_subscription_stats(&fixture("subscription_stats_sample.json")).unwrap();
    // amountUsedRatio 0.1612 → 总已用 16.12%；kimiCodeUsedRatio 0.05 → Code 5%
    assert!((m.total_pct - 16.12).abs() < 1e-9);
    assert!((m.code_pct - 5.0).abs() < 1e-9);
    // Kimi 部分 = total - code = 11.12%
    assert!((m.kimi_pct - 11.12).abs() < 1e-9);
    // expireTime 原样透传
    assert_eq!(m.reset_time.as_deref(), Some("2026-08-01T00:00:00Z"));
}

#[test]
fn snake_case_variant_parses_identically() {
    let m = parse_subscription_stats(&fixture("subscription_stats_snake_case.json")).unwrap();
    assert!((m.total_pct - 40.0).abs() < 1e-9);
    assert!((m.code_pct - 10.0).abs() < 1e-9);
    assert!((m.kimi_pct - 30.0).abs() < 1e-9);
    assert_eq!(m.reset_time.as_deref(), Some("2026-08-01T00:00:00Z"));
}

#[test]
fn monthly_info_serializes_snake_case_and_skips_none() {
    // 与 src/types.ts 契约一致：snake_case 字段名；reset_time 缺失时不输出该键
    let m = MonthlyInfo {
        total_pct: 16.12,
        kimi_pct: 11.12,
        code_pct: 5.0,
        reset_time: None,
    };
    let json = serde_json::to_string(&m).unwrap();
    assert!(json.contains("\"total_pct\""));
    assert!(json.contains("\"kimi_pct\""));
    assert!(json.contains("\"code_pct\""));
    assert!(!json.contains("reset_time"));

    let m = MonthlyInfo {
        reset_time: Some("2026-08-01T00:00:00Z".to_string()),
        ..m
    };
    let json = serde_json::to_string(&m).unwrap();
    assert!(json.contains("\"reset_time\":\"2026-08-01T00:00:00Z\""));
}

#[test]
fn error_display_is_chinese() {
    // 前端直接展示 Display 文本，确认是中文（Parse 分支会原样上屏）
    let err = parse_subscription_stats("{}").unwrap_err();
    assert!(matches!(err, WebError::Parse(_)));
    assert!(err.to_string().contains("月度数据解析失败"));
    assert_eq!(
        WebError::Unauthorized.to_string(),
        "网页登录态无效或已过期 (HTTP 401/403)"
    );
}
