//! 智谱 GLM Coding Plan 接入层：套餐额度查询（`GET /api/monitor/usage/quota/limit`）。
//!
//! 与 kimi/ 、deepseek/ 并列的第三种账号数据源：凭证为开放平台 API Key
//! （无固定前缀，线上为 `id.secret` 点分两段，复用 `api_key/<账号id>` keyring 槽位）。
//! 只做国内端点（open.bigmodel.cn）；额度映射进现有 KimiQuota 契约
//! （five_hour / weekly / membership_level），无月度/总额/Booster 概念。

pub mod client;
pub mod models;

/// 额度查询接口 base（仅国内端点，GOAL 拍板）
pub const API_BASE: &str = "https://open.bigmodel.cn";
/// HTTP 超时（秒），与 deepseek 一致取 15 秒（短于 kimi 的 30 秒）
pub const HTTP_TIMEOUT_SECS: u64 = 15;
