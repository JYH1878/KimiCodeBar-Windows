//! Kimi 官方 API 接入层：用量查询与 OAuth 设备码登录。
//!
//! 语义对齐 macOS 版 KimiCodeBar：两种凭证（API Key / OAuth access_token）
//! 均调用同一个 `GET /coding/v1/usages` 接口。

pub mod client;
pub mod dpapi;
pub mod models;
pub mod oauth;
pub mod web;

/// 用量查询接口 base（Mac 版与 Windows 移植版一致）
pub const API_BASE: &str = "https://api.kimi.com";
/// OAuth 设备码授权 host（与 Kimi Code CLI 官方 oauth 包一致）
pub const AUTH_BASE: &str = "https://auth.kimi.com";
/// OAuth client_id（沿用 Mac 版 / CLI）
pub const OAUTH_CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
/// 请求 UA
pub const USER_AGENT: &str = "KimiCodeBar/1.0";
/// HTTP 超时（秒），与 Mac 版一致
pub const HTTP_TIMEOUT_SECS: u64 = 30;
