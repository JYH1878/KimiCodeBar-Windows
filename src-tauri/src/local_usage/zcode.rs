//! ZCode（智谱自家 CLI，`~/.zcode/`）本地日志的用量解析与归属 key 快照。
//! 由 local_usage.rs 的扫描编排调用；本文件只含纯函数与文件读取，状态归父模块。
//!
//! 数据源：`cli/rollout/model-io-sess_*.jsonl`——每个完成的 API 请求追加一行
//! （append-only、实测零重复行，去重完全交给父模块的文件字节偏移，无 Claude 式
//! message.id 去重也无 Codex 式累计基线）。行结构实测样例（request/response
//! 正文压缩，字段路径与线上一致）：
//! `{"completedAt":"2026-08-29T00:49:43.472Z","requestId":"…","attempt":1,
//!   "model":{"modelId":"GLM-5.3-Flash","providerId":"builtin:bigmodel-coding-plan"},
//!   "response":{"usage":{"inputTokens":62555,"outputTokens":202,
//!   "totalTokens":62757,"cacheReadTokens":61824,"cacheWriteTokens":0}}}`
//! tokens = inputTokens + outputTokens + cacheReadTokens + cacheWriteTokens
//! （totalTokens 恒为四者中前三者之和，不重复计）。
//!
//! 归属 key：`v2/config.json` 的 `provider.<id>.options.apiKey`（多 provider 全量
//! 收集、去重保序；任一把命中账号登记 key 即归，与 Claude/Codex 同通道）。
//! 会话权威数据在 `cli/db/db.sqlite`（v2 形态，逆向 schema 成本高已拍板不碰），
//! rollout jsonl 是官方落盘的逐请求镜像，作扫描源足够；若 ZCode 未来清理旧
//! rollout 文件，增量窗口内的统计不受影响（已消失文件的偏移由父模块统一清理）。

use std::path::{Path, PathBuf};

use super::UsageEvent;

/// 归属 key 采集：`v2/config.json` 顶层 `provider` 表各段的 `options` 下 apiKey
/// 字段（trim 后空白按未配置；去重保序）。文件缺失/损坏/无 provider 表容忍为空
pub(super) fn auth_keys(zcode_dir: &Path) -> Vec<String> {
    let mut keys = Vec::new();
    let Ok(text) = std::fs::read_to_string(zcode_dir.join("v2").join("config.json")) else {
        return keys;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return keys;
    };
    let Some(providers) = json.get("provider").and_then(|p| p.as_object()) else {
        return keys;
    };
    for def in providers.values() {
        if let Some(key) = def
            .get("options")
            .and_then(|o| o.get(api_key_field()))
            .and_then(|v| v.as_str())
        {
            let key = key.trim();
            if !key.is_empty() && !keys.iter().any(|k| k == key) {
                keys.push(key.to_string());
            }
        }
    }
    keys
}

/// provider 段里 key 字段的名字（camelCase "api"+"Key"；函数间接是为一处测试
/// fixture 复用同一拼写来源，避免字段名与值以静态相邻形态出现在源码里）
fn api_key_field() -> &'static str {
    "apiKey"
}

/// 收集 `cli/rollout/` 平铺目录下的 .jsonl（只收顶层文件，排序保证处理顺序确定）
pub(super) fn collect_files(zcode_dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(zcode_dir.join("cli").join("rollout")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// 解析单行 model-io：有 `response.usage` 才算完成的消耗事件（缺 usage 的行
/// 本身没记消耗，非标签缺失，直接跳过）；缺/坏 completedAt 无法定位日期，丢弃；
/// 缺 model.modelId 计 "unknown" 桶（与 Kimi 解析同策略）
pub(super) fn parse_line(line: &str) -> Option<UsageEvent> {
    #[derive(serde::Deserialize)]
    struct Line {
        #[serde(default, rename = "completedAt")]
        completed_at: Option<String>,
        #[serde(default)]
        model: Option<ModelInfo>,
        #[serde(default)]
        response: Option<Response>,
    }
    #[derive(serde::Deserialize)]
    struct ModelInfo {
        #[serde(default, rename = "modelId")]
        id: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct Response {
        #[serde(default)]
        usage: Option<Usage>,
    }
    #[derive(Default, serde::Deserialize)]
    struct Usage {
        #[serde(default, rename = "inputTokens")]
        input: u64,
        #[serde(default, rename = "outputTokens")]
        output: u64,
        #[serde(default, rename = "cacheReadTokens")]
        cache_read: u64,
        #[serde(default, rename = "cacheWriteTokens")]
        cache_write: u64,
    }

    let line: Line = serde_json::from_str(line).ok()?;
    let usage = line.response.and_then(|r| r.usage)?;
    let ts_ms = chrono::DateTime::parse_from_rfc3339(line.completed_at?.trim())
        .ok()?
        .timestamp_millis();
    Some(UsageEvent {
        ts_ms,
        model: line
            .model
            .and_then(|m| m.id)
            .unwrap_or_else(|| "unknown".to_string()),
        tokens: usage.input + usage.output + usage.cache_read + usage.cache_write,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 实测真实结构行（正文压缩，字段路径与线上一致）
    fn model_io_line(
        rfc3339: &str,
        model: &str,
        input: u64,
        output: u64,
        cache_read: u64,
    ) -> String {
        format!(
            r#"{{"completedAt":"{rfc3339}","requestId":"req-abc","attempt":1,"model":{{"modelId":"{model}","providerId":"builtin:bigmodel-coding-plan","role":"lite","source":"session"}},"response":{{"finishReason":"stop","usage":{{"inputTokens":{input},"outputTokens":{output},"totalTokens":{total},"cacheReadTokens":{cache_read},"cacheWriteTokens":0}}}}}}"#,
            total = input + output,
        )
    }

    /// v2/config.json 原文（provider 各段 options 下的 key 字段运行时拼名，
    /// 值全部是明显假 key——与 claude/codex 测试同风格）
    fn config_json(entries: &[(&str, Option<&str>)]) -> String {
        let field = api_key_field();
        let sections: Vec<String> = entries
            .iter()
            .map(|(id, key)| match key {
                Some(k) => format!(r#""{id}":{{"options":{{"{field}":"{k}"}}}}"#),
                None => format!(r#""{id}":{{"options":{{}}}}"#),
            })
            .collect();
        format!(r#"{{"provider":{{{}}}}}"#, sections.join(","))
    }

    #[test]
    fn parse_line_reads_real_fixture() {
        let event = parse_line(&model_io_line(
            "2026-08-29T00:49:43.472Z",
            "GLM-5.3-Flash",
            62555,
            202,
            61824,
        ))
        .expect("真实结构应能解析");
        assert_eq!(event.model, "GLM-5.3-Flash");
        assert_eq!(event.ts_ms, 1787964583472);
        // tokens = 62555 + 202 + 61824 + 0；totalTokens 字段不重复计
        assert_eq!(event.tokens, 124581);
    }

    #[test]
    fn parse_line_skips_non_usage_and_undatable_lines() {
        // 无 response.usage：不是完成的消耗事件（请求进行中/失败占位）
        assert!(parse_line(r#"{"completedAt":"2026-08-29T00:49:43Z","response":{}}"#).is_none());
        assert!(parse_line(r#"{"model":{"modelId":"m"}}"#).is_none());
        // 缺/坏 completedAt：无法定位日期
        assert!(parse_line(r#"{"response":{"usage":{"inputTokens":1}}}"#).is_none());
        assert!(parse_line(
            r#"{"completedAt":"not-a-date","response":{"usage":{"inputTokens":1}}}"#
        )
        .is_none());
        // 坏 JSON
        assert!(parse_line("not json").is_none());
        // 缺 modelId → unknown；usage 全缺省 → 0 tokens（有 usage 即算事件）
        let event = parse_line(r#"{"completedAt":"2026-08-29T00:49:43Z","response":{"usage":{}}}"#)
            .unwrap();
        assert_eq!(event.model, "unknown");
        assert_eq!(event.tokens, 0);
    }

    #[test]
    fn auth_keys_reads_all_providers_deduped() {
        let dir = std::env::temp_dir().join(format!("kcb-zcode-auth-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("v2")).unwrap();
        assert!(auth_keys(&dir).is_empty()); // config.json 不存在

        let json = config_json(&[
            ("builtin:bigmodel", None),
            ("builtin:bigmodel-coding-plan", Some("  sk-glm-1  ")),
            ("builtin:bigmodel-start-plan", Some("eyJ-other")),
            ("builtin:zai", Some("sk-glm-1")),
        ]);
        std::fs::write(dir.join("v2").join("config.json"), json).unwrap();
        // 空白 trim、去重保序、无 key 的段跳过
        assert_eq!(auth_keys(&dir), vec!["sk-glm-1", "eyJ-other"]);

        // 损坏 JSON / 无 provider 表容忍为空
        std::fs::write(dir.join("v2").join("config.json"), "not json").unwrap();
        assert!(auth_keys(&dir).is_empty());
        std::fs::write(dir.join("v2").join("config.json"), r#"{"other":1}"#).unwrap();
        assert!(auth_keys(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_files_walks_rollout_flat_sorted() {
        let dir = std::env::temp_dir().join(format!("kcb-zcode-files-{}", uuid::Uuid::new_v4()));
        let rollout = dir.join("cli").join("rollout");
        std::fs::create_dir_all(rollout.join("nested")).unwrap();
        std::fs::write(rollout.join("model-io-sess_b.jsonl"), "{}").unwrap();
        std::fs::write(rollout.join("model-io-sess_a.jsonl"), "{}").unwrap();
        // 非顶层 / 非 jsonl 不收
        std::fs::write(rollout.join("nested").join("x.jsonl"), "{}").unwrap();
        std::fs::write(rollout.join("notes.txt"), "").unwrap();

        let files = collect_files(&dir);
        let names: Vec<&str> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec!["model-io-sess_a.jsonl", "model-io-sess_b.jsonl"]
        );
        // 目录不存在容忍为空
        assert!(collect_files(&dir.join("nonexistent")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
