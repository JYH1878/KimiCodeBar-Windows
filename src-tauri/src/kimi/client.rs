//! 用量查询 HTTP 客户端：API Key 与 OAuth token 通用（服务端不区分）。

use std::time::Duration;

use crate::kimi::models::UsageResponse;
use crate::kimi::{API_BASE, HTTP_TIMEOUT_SECS, USER_AGENT};
use crate::quota::{self, KimiQuota, QuotaError};

/// Kimi 用量查询客户端。
///
/// 注意：本结构体的公开签名（`new` / `fetch_usages` / `fetch_quota` 及其
/// 参数、返回类型）由架构方在 stub 阶段定死，填实现时一律不许改动。
pub struct KimiClient {
    http: reqwest::Client,
    base_url: String,
}

impl Default for KimiClient {
    fn default() -> Self {
        Self::new()
    }
}

impl KimiClient {
    /// 30s 超时、UA `KimiCodeBar/1.0`（常量见 crate::kimi）
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
            .user_agent(USER_AGENT)
            // 凭证走明文 HTTP 不可接受，只允许 HTTPS
            .https_only(true)
            .build()
            .expect("构建 reqwest::Client 失败");
        Self {
            http,
            base_url: API_BASE.to_string(),
        }
    }

    /// GET /coding/v1/usages，返回 wire 模型。
    /// 401/403 → QuotaError::Unauthorized；其他非 2xx → Api（抽取 error/message/detail）；
    /// 网络失败 → Http。
    pub async fn fetch_usages(&self, token: &str) -> Result<UsageResponse, QuotaError> {
        let body = self.get_usages_body(token).await?;
        serde_json::from_str(&body).map_err(|e| QuotaError::Parse(e.to_string()))
    }

    /// fetch_usages + 解析为领域模型的便捷方法。
    pub async fn fetch_quota(&self, token: &str) -> Result<KimiQuota, QuotaError> {
        // 原始响应只用于诊断导出，常规刷新直接丢弃
        self.fetch_quota_with_raw(token)
            .await
            .map(|(quota, _)| quota)
    }

    /// 与 fetch_quota 相同，但同时返回响应原文（诊断导出用）。
    pub async fn fetch_quota_with_raw(
        &self,
        token: &str,
    ) -> Result<(KimiQuota, String), QuotaError> {
        // 只请求一次：拿到原始文本后先做 wire 层反序列化校验
        //（与 fetch_usages 的 Parse 错误语义一致），再解析为领域模型。
        let body = self.get_usages_body(token).await?;
        serde_json::from_str::<UsageResponse>(&body)
            .map_err(|e| QuotaError::Parse(e.to_string()))?;
        let quota = quota::parse_usage(&body)?;
        Ok((quota, body))
    }

    /// 实际发起 GET 请求并完成状态码映射，2xx 时返回响应原文。
    async fn get_usages_body(&self, token: &str) -> Result<String, QuotaError> {
        let resp = self
            .http
            .get(format!("{}/coding/v1/usages", self.base_url))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| {
                tracing::warn!("usages 请求发送失败: {e}");
                QuotaError::Http(e.to_string())
            })?;

        let status = resp.status();
        let body = resp.text().await.map_err(|e| {
            tracing::warn!("usages 响应读取失败: {e}");
            QuotaError::Http(e.to_string())
        })?;

        if status.is_success() {
            return Ok(body);
        }
        // 错误分支只记状态码与响应体长度，严禁记录 token / 响应原文
        tracing::warn!(
            "usages 接口返回非 2xx: status={}, body_len={}",
            status.as_u16(),
            body.len()
        );
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(QuotaError::Unauthorized);
        }
        Err(QuotaError::Api {
            status: status.as_u16(),
            message: extract_error_message(&body),
        })
    }
}

/// 从错误响应体中抽取可读错误信息（纯函数，便于单测）。
///
/// 依次尝试 JSON 的 `error` 字段（可能是字符串，也可能是含 `message` 的对象）、
/// `message`、`detail` 字段；全部落空则兜底返回线性截断到 200 字符的原文。
fn extract_error_message(body: &str) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(error) = value.get("error") {
            if let Some(msg) = error.as_str() {
                return msg.to_string();
            }
            if let Some(msg) = error.get("message").and_then(|m| m.as_str()) {
                return msg.to_string();
            }
        }
        for key in ["message", "detail"] {
            if let Some(msg) = value.get(key).and_then(|m| m.as_str()) {
                return msg.to_string();
            }
        }
    }
    // 兜底：线性截断原文到 200 字符（按 char 取，避免切断 UTF-8 序列）
    body.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::extract_error_message;

    #[test]
    fn error_string() {
        let body = r#"{"error":"invalid token"}"#;
        assert_eq!(extract_error_message(body), "invalid token");
    }

    #[test]
    fn error_object_with_message() {
        let body = r#"{"error":{"code":"rate_limited","message":"too many requests"}}"#;
        assert_eq!(extract_error_message(body), "too many requests");
    }

    #[test]
    fn message_field() {
        let body = r#"{"message":"quota service unavailable"}"#;
        assert_eq!(extract_error_message(body), "quota service unavailable");
    }

    #[test]
    fn detail_field() {
        let body = r#"{"detail":"token expired at 2026-01-01"}"#;
        assert_eq!(extract_error_message(body), "token expired at 2026-01-01");
    }

    #[test]
    fn non_json_fallback() {
        let body = "<html><body>502 Bad Gateway</body></html>";
        assert_eq!(extract_error_message(body), body);
    }

    #[test]
    fn fallback_truncates_to_200_chars() {
        let body = "x".repeat(500);
        assert_eq!(extract_error_message(&body).chars().count(), 200);
    }
}
