//! DeepSeek 余额查询 HTTP 客户端：开放平台 API Key（sk- 前缀）作 Bearer。

use std::time::Duration;

use crate::deepseek::models::{self, DeepSeekBalance};
use crate::deepseek::{API_BASE, HTTP_TIMEOUT_SECS};
use crate::kimi::USER_AGENT;
use crate::quota::QuotaError;

/// DeepSeek 余额查询客户端（仿 kimi::client::KimiClient 的构造与错误映射）
pub struct DeepSeekClient {
    http: reqwest::Client,
    base_url: String,
}

impl Default for DeepSeekClient {
    fn default() -> Self {
        Self::new()
    }
}

impl DeepSeekClient {
    /// 15s 超时（GOAL 拍板）、UA 与 kimi 一致、凭证走明文 HTTP 不可接受（仅 HTTPS）
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
            .user_agent(USER_AGENT)
            .https_only(true)
            .build()
            .expect("构建 reqwest::Client 失败");
        Self {
            http,
            base_url: API_BASE.to_string(),
        }
    }

    /// GET /user/balance，解析为余额领域模型。
    /// 401/403 → Unauthorized（key 无效）；其他非 2xx（含 429 限流）→ Api；网络失败 → Http。
    pub async fn fetch_balance(&self, key: &str) -> Result<DeepSeekBalance, QuotaError> {
        let body = self.get_balance_body(key).await?;
        models::parse_balance(&body)
    }

    /// 实际发起 GET 请求并完成状态码映射，2xx 时返回响应原文。
    async fn get_balance_body(&self, key: &str) -> Result<String, QuotaError> {
        let resp = self
            .http
            .get(format!("{}/user/balance", self.base_url))
            .bearer_auth(key)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| {
                tracing::warn!("balance 请求发送失败: {e}");
                QuotaError::Http(e.to_string())
            })?;

        let status = resp.status();
        let body = resp.text().await.map_err(|e| {
            tracing::warn!("balance 响应读取失败: {e}");
            QuotaError::Http(e.to_string())
        })?;

        if status.is_success() {
            return Ok(body);
        }
        // 错误分支只记状态码与响应体长度，严禁记录 key / 响应原文
        tracing::warn!(
            "balance 接口返回非 2xx: status={}, body_len={}",
            status.as_u16(),
            body.len()
        );
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(QuotaError::Unauthorized);
        }
        Err(QuotaError::Api {
            status: status.as_u16(),
            // 线性截断到 200 字符（按 char 取，避免切断 UTF-8 序列）
            message: body.chars().take(200).collect(),
        })
    }
}
