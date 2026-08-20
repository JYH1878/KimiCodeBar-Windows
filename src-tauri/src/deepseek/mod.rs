//! DeepSeek 开放平台接入层：余额查询（`GET /user/balance`）。
//!
//! 与 kimi/ 并列的第二种账号数据源：凭证为开放平台 API Key（sk- 前缀，
//! 复用 `api_key/<账号id>` keyring 槽位），只查余额，无配额/月度/历史概念。

pub mod client;
pub mod models;

/// 余额查询接口 base
pub const API_BASE: &str = "https://api.deepseek.com";
/// HTTP 超时（秒），GOAL 拍板 15 秒（短于 kimi 的 30 秒）
pub const HTTP_TIMEOUT_SECS: u64 = 15;
