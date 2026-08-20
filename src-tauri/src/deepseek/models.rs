//! DeepSeek 余额的 wire 模型与领域模型：`GET /user/balance` 响应 → 面板契约对象。
//!
//! 接口返回（金额均为字符串）：
//! `{"is_available":true,"balance_infos":[{"currency":"CNY","total_balance":"12.34",
//! "granted_balance":"2.00","topped_up_balance":"10.34"}]}`
//! 多币种取 CNY，无 CNY 取第一个总余额非零的币种，不跨币种求和（GOAL 拍板）。

use serde::{Deserialize, Serialize};

use crate::quota::QuotaError;

/// 余额接口 wire 响应
#[derive(Debug, Deserialize)]
pub struct BalanceResponseWire {
    /// 账户是否可用（false 视为低额，走红点 + 托盘红）
    #[serde(default)]
    pub is_available: bool,
    #[serde(default)]
    pub balance_infos: Vec<BalanceInfoWire>,
}

/// 单币种余额（金额是字符串，如 "12.34"）
#[derive(Debug, Deserialize)]
pub struct BalanceInfoWire {
    #[serde(default)]
    pub currency: String,
    #[serde(default)]
    pub total_balance: String,
    #[serde(default)]
    pub granted_balance: String,
    #[serde(default)]
    pub topped_up_balance: String,
}

/// 面板契约对象（与 src/types.ts 的 DeepSeekBalance 一一对应，snake_case；金额单位元，f64）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeepSeekBalance {
    /// 账户是否可用（false 面板显示不可用横幅并计为低额）
    pub is_available: bool,
    /// 币种（如 "CNY" / "USD"）
    pub currency: String,
    /// 总余额（元）
    pub total_balance: f64,
    /// 其中赠金余额（元）
    pub granted_balance: f64,
    /// 其中充值余额（元）
    pub topped_up_balance: f64,
}

/// 解析 `/user/balance` 响应 JSON 为领域模型：金额字符串转 f64（非法按 0），
/// 多币种按「CNY → 第一个非零 → 第一个」挑选，空列表产出零值余额。
pub fn parse_balance(json: &str) -> Result<DeepSeekBalance, QuotaError> {
    let resp: BalanceResponseWire =
        serde_json::from_str(json).map_err(|e| QuotaError::Parse(e.to_string()))?;
    Ok(pick_balance(resp.is_available, &resp.balance_infos))
}

/// 多币种挑选（纯函数便于单测）：CNY 优先，否则第一个总余额非零，再否则第一个；
/// 空列表按 CNY 零值兜底
fn pick_balance(is_available: bool, infos: &[BalanceInfoWire]) -> DeepSeekBalance {
    let picked = infos
        .iter()
        .find(|i| i.currency == "CNY")
        .or_else(|| infos.iter().find(|i| parse_amount(&i.total_balance) != 0.0))
        .or_else(|| infos.first());
    match picked {
        Some(info) => DeepSeekBalance {
            is_available,
            currency: info.currency.clone(),
            total_balance: parse_amount(&info.total_balance),
            granted_balance: parse_amount(&info.granted_balance),
            topped_up_balance: parse_amount(&info.topped_up_balance),
        },
        None => DeepSeekBalance {
            is_available,
            currency: "CNY".to_string(),
            total_balance: 0.0,
            granted_balance: 0.0,
            topped_up_balance: 0.0,
        },
    }
}

/// 金额字符串 → f64；空白/非法按 0（与 quota::parse_num 的容忍语义一致）
fn parse_amount(s: &str) -> f64 {
    s.trim().parse::<f64>().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_string_amounts_to_f64() {
        let json = r#"{"is_available":true,"balance_infos":[{"currency":"CNY","total_balance":"12.34","granted_balance":"2.00","topped_up_balance":"10.34"}]}"#;
        let b = parse_balance(json).unwrap();
        assert!(b.is_available);
        assert_eq!(b.currency, "CNY");
        assert!((b.total_balance - 12.34).abs() < 1e-9);
        assert!((b.granted_balance - 2.0).abs() < 1e-9);
        assert!((b.topped_up_balance - 10.34).abs() < 1e-9);
    }

    #[test]
    fn multi_currency_prefers_cny() {
        // 多币种取 CNY，即使它排在后面且金额更小（不跨币种求和）
        let json = r#"{"is_available":true,"balance_infos":[{"currency":"USD","total_balance":"99.00","granted_balance":"0.00","topped_up_balance":"99.00"},{"currency":"CNY","total_balance":"3.20","granted_balance":"1.00","topped_up_balance":"2.20"}]}"#;
        let b = parse_balance(json).unwrap();
        assert_eq!(b.currency, "CNY");
        assert!((b.total_balance - 3.20).abs() < 1e-9);
    }

    #[test]
    fn no_cny_picks_first_nonzero() {
        // 无 CNY：取第一个总余额非零的币种
        let json = r#"{"is_available":true,"balance_infos":[{"currency":"EUR","total_balance":"0.00","granted_balance":"0.00","topped_up_balance":"0.00"},{"currency":"USD","total_balance":"8.50","granted_balance":"8.50","topped_up_balance":"0.00"}]}"#;
        let b = parse_balance(json).unwrap();
        assert_eq!(b.currency, "USD");
        assert!((b.total_balance - 8.50).abs() < 1e-9);
        assert!((b.granted_balance - 8.50).abs() < 1e-9);
    }

    #[test]
    fn no_cny_all_zero_picks_first() {
        let json = r#"{"is_available":true,"balance_infos":[{"currency":"USD","total_balance":"0.00","granted_balance":"0.00","topped_up_balance":"0.00"}]}"#;
        let b = parse_balance(json).unwrap();
        assert_eq!(b.currency, "USD");
        assert_eq!(b.total_balance, 0.0);
    }

    #[test]
    fn is_available_false_propagates() {
        // 余额不可用：标志原样透出（上层据此判低额 + 显示不可用横幅）
        let json = r#"{"is_available":false,"balance_infos":[{"currency":"CNY","total_balance":"0.00","granted_balance":"0.00","topped_up_balance":"0.00"}]}"#;
        let b = parse_balance(json).unwrap();
        assert!(!b.is_available);
    }

    #[test]
    fn empty_balance_infos_yields_zero_cny() {
        let b = parse_balance(r#"{"is_available":true,"balance_infos":[]}"#).unwrap();
        assert!(b.is_available);
        assert_eq!(b.currency, "CNY");
        assert_eq!(b.total_balance, 0.0);
    }

    #[test]
    fn invalid_amount_string_tolerated_as_zero() {
        let json = r#"{"is_available":true,"balance_infos":[{"currency":"CNY","total_balance":"abc","granted_balance":"","topped_up_balance":" 10.5 "}]}"#;
        let b = parse_balance(json).unwrap();
        assert_eq!(b.total_balance, 0.0);
        assert_eq!(b.granted_balance, 0.0);
        assert!((b.topped_up_balance - 10.5).abs() < 1e-9);
    }

    #[test]
    fn invalid_json_is_parse_error() {
        assert!(matches!(
            parse_balance("not json"),
            Err(QuotaError::Parse(_))
        ));
    }
}
