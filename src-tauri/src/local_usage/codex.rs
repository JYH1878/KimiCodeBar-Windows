//! Codex 本地日志（`~/.codex/sessions/**/*.jsonl` + `archived_sessions/*.jsonl`）解析与归属快照。
//! 由 local_usage.rs 的扫描编排调用；本文件只含纯函数与文件读取，状态归父模块。

use std::path::{Path, PathBuf};

use super::{CodexTotals, UsageEvent};

/// 归属 key 采集：`auth.json` 的 OPENAI_API_KEY + `config.toml` 当前
/// `model_provider` 段的 `experimental_bearer_token`（去重，任一匹配即归）。
/// OAuth 形态的 Codex 登录两处皆无，自然落未归属（设计内）
pub(super) fn auth_keys(codex_dir: &Path) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    if let Ok(text) = std::fs::read_to_string(codex_dir.join("auth.json")) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(key) = json.get("OPENAI_API_KEY").and_then(|v| v.as_str()) {
                push_key(&mut keys, key);
            }
        }
    }
    if let Ok(text) = std::fs::read_to_string(codex_dir.join("config.toml")) {
        if let Ok(doc) = text.parse::<toml::Table>() {
            let provider = doc.get("model_provider").and_then(|v| v.as_str());
            if let Some(provider) = provider {
                let section = doc
                    .get("model_providers")
                    .and_then(|v| v.get(provider))
                    .and_then(|v| v.as_table());
                if let Some(token) = section
                    .and_then(|s| s.get("experimental_bearer_token"))
                    .and_then(|v| v.as_str())
                {
                    push_key(&mut keys, token);
                }
            }
        }
    }
    keys
}

fn push_key(keys: &mut Vec<String>, key: &str) {
    let key = key.trim();
    if !key.is_empty() && !keys.iter().any(|k| k == key) {
        keys.push(key.to_string());
    }
}

/// `sessions/` 递归（日期分区 YYYY/MM/DD）+ `archived_sessions/` 平铺，排序保证确定性
pub(super) fn collect_files(codex_dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_recursive(&codex_dir.join("sessions"), &mut files);
    if let Ok(entries) = std::fs::read_dir(codex_dir.join("archived_sessions")) {
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

fn collect_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_recursive(&path, out);
        } else if file_type.is_file() && path.extension().and_then(|e| e.to_str()) == Some("jsonl")
        {
            out.push(path);
        }
    }
}

/// 解析本批新行出账（文件级状态由父模块持有、跨扫描持久）：
/// - `turn_context` 行刷新该文件当前模型（model 字段按日志原样保留，无则 "unknown"）；
/// - `event_msg`/`token_count` 事件优先用 `info.last_token_usage`（单轮精确值，
///   input+output+cached_input+reasoning_output 四字段求和）；缺失时用
///   `info.total_token_usage`（累计快照）按该文件上次累计差分（负差/重复快照 = 0）。
///
/// 时间 = 顶层 timestamp（RFC3339）；零 token 事件不出（无计费意义）
pub(super) fn settle_new_lines(
    lines: &[String],
    model: &mut Option<String>,
    totals: &mut CodexTotals,
) -> Vec<UsageEvent> {
    let mut events = Vec::new();
    for line in lines {
        // 快速预过滤：只关心 event_msg(token_count) 与 turn_context，其余行直接跳过
        if !line.contains("\"event_msg\"") && !line.contains("\"turn_context\"") {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match value
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
        {
            "turn_context" => {
                if let Some(m) = value
                    .get("payload")
                    .and_then(|p| p.get("model"))
                    .and_then(|v| v.as_str())
                {
                    *model = Some(m.to_string());
                }
            }
            "event_msg" => {
                let Some(payload) = value.get("payload") else {
                    continue;
                };
                if payload.get("type").and_then(|v| v.as_str()) != Some("token_count") {
                    continue;
                }
                let Some(info) = payload.get("info").filter(|v| !v.is_null()) else {
                    continue;
                };
                let Some(ts_ms) = value
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .and_then(|t| chrono::DateTime::parse_from_rfc3339(t.trim()).ok())
                    .map(|dt| dt.timestamp_millis())
                else {
                    continue;
                };
                let tokens = read_tokens(info, totals);
                if tokens > 0 {
                    events.push(UsageEvent {
                        ts_ms,
                        model: model.clone().unwrap_or_else(|| "unknown".to_string()),
                        tokens,
                    });
                }
            }
            _ => {}
        }
    }
    events
}

/// 单个 token_count 事件的 tokens：优先 last_token_usage 四字段精确值；
/// 否则 total_token_usage 三字段累计差分（差分基线按分量 max 推进，防快照回退重复计）
fn read_tokens(info: &serde_json::Value, last: &mut CodexTotals) -> u64 {
    if let Some(last_usage) = info.get("last_token_usage").filter(|v| !v.is_null()) {
        let field = |name: &str| last_usage.get(name).and_then(|v| v.as_u64()).unwrap_or(0);
        return field("input_tokens")
            + field("output_tokens")
            + field("cached_input_tokens")
            + field("reasoning_output_tokens");
    }
    let Some(total) = info.get("total_token_usage").filter(|v| !v.is_null()) else {
        return 0;
    };
    let field = |name: &str| total.get(name).and_then(|v| v.as_u64()).unwrap_or(0);
    let current = CodexTotals {
        input: field("input_tokens"),
        cached: field("cached_input_tokens"),
        output: field("output_tokens"),
    };
    let delta = current.diff(last);
    last.merge_max(&current);
    delta
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实格式 token_count 行（last_token_usage 单轮精确值形态）
    fn token_count_line(
        rfc3339: &str,
        last: Option<&[(&str, u64); 4]>,
        total: Option<&[(&str, u64); 3]>,
    ) -> String {
        let mut info = serde_json::Map::new();
        if let Some(fields) = last {
            info.insert("last_token_usage".into(), fields_json4(fields));
        }
        if let Some(fields) = total {
            info.insert("total_token_usage".into(), fields_json3(fields));
        }
        let payload = serde_json::json!({
            "type": "token_count",
            "info": serde_json::Value::Object(info),
        });
        serde_json::json!({
            "timestamp": rfc3339,
            "type": "event_msg",
            "payload": payload,
        })
        .to_string()
    }

    fn fields_json4(fields: &[(&str, u64); 4]) -> serde_json::Value {
        serde_json::json!({
            fields[0].0: fields[0].1,
            fields[1].0: fields[1].1,
            fields[2].0: fields[2].1,
            fields[3].0: fields[3].1,
        })
    }

    fn fields_json3(fields: &[(&str, u64); 3]) -> serde_json::Value {
        serde_json::json!({
            fields[0].0: fields[0].1,
            fields[1].0: fields[1].1,
            fields[2].0: fields[2].1,
        })
    }

    fn turn_context_line(model: &str) -> String {
        serde_json::json!({
            "timestamp": "2026-05-01T10:00:00.000Z",
            "type": "turn_context",
            "payload": {"model": model, "cwd": "/tmp"},
        })
        .to_string()
    }

    #[test]
    fn settle_prefers_last_token_usage_exact_values() {
        let mut model = None;
        let mut totals = CodexTotals::default();
        let lines = vec![
            turn_context_line("gpt-5.4-codex"),
            // last_token_usage：3+150+500+40 = 693
            token_count_line(
                "2026-05-01T10:00:01.000Z",
                Some(&[
                    ("input_tokens", 3),
                    ("output_tokens", 150),
                    ("cached_input_tokens", 500),
                    ("reasoning_output_tokens", 40),
                ]),
                // total 快照在场也被忽略（last 优先）
                Some(&[
                    ("input_tokens", 999),
                    ("cached_input_tokens", 0),
                    ("output_tokens", 0),
                ]),
            ),
        ];
        let events = settle_new_lines(&lines, &mut model, &mut totals);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].tokens, 693);
        assert_eq!(events[0].model, "gpt-5.4-codex");
        assert_eq!(
            events[0].ts_ms,
            chrono::DateTime::parse_from_rfc3339("2026-05-01T10:00:01.000Z")
                .unwrap()
                .timestamp_millis()
        );
    }

    #[test]
    fn settle_diffs_total_snapshots_across_events() {
        let mut model = None;
        let mut totals = CodexTotals::default();
        // 只给累计快照：首轮 100+10+50 = 160 全计
        let lines = vec![token_count_line(
            "2026-05-01T10:00:01.000Z",
            None,
            Some(&[
                ("input_tokens", 100),
                ("cached_input_tokens", 10),
                ("output_tokens", 50),
            ]),
        )];
        let events = settle_new_lines(&lines, &mut model, &mut totals);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].tokens, 160);
        assert_eq!(events[0].model, "unknown");

        // 快照推进到 120/10/80：差分 = 20 + 0 + 30 = 50
        let lines2 = vec![token_count_line(
            "2026-05-01T10:00:02.000Z",
            None,
            Some(&[
                ("input_tokens", 120),
                ("cached_input_tokens", 10),
                ("output_tokens", 80),
            ]),
        )];
        let events2 = settle_new_lines(&lines2, &mut model, &mut totals);
        assert_eq!(events2.len(), 1);
        assert_eq!(events2[0].tokens, 50);

        // 重复快照（限流刷新重发）：差分 0，不出事件
        let lines3 = vec![token_count_line(
            "2026-05-01T10:00:03.000Z",
            None,
            Some(&[
                ("input_tokens", 120),
                ("cached_input_tokens", 10),
                ("output_tokens", 80),
            ]),
        )];
        assert!(settle_new_lines(&lines3, &mut model, &mut totals).is_empty());

        // 快照回退（分量变小）：负差丢弃，基线不回退
        let lines4 = vec![token_count_line(
            "2026-05-01T10:00:04.000Z",
            None,
            Some(&[
                ("input_tokens", 50),
                ("cached_input_tokens", 0),
                ("output_tokens", 20),
            ]),
        )];
        assert!(settle_new_lines(&lines4, &mut model, &mut totals).is_empty());
    }

    #[test]
    fn settle_tracks_model_from_turn_context_and_skips_noise() {
        let mut model = None;
        let mut totals = CodexTotals::default();
        let lines = vec![
            // 噪声行：非 event_msg/turn_context
            r#"{"timestamp":"2026-05-01T10:00:00.000Z","type":"session_meta","payload":{"id":"t1"}}"#.to_string(),
            // token_count 但非 event_msg 外壳（直接是别的类型）
            token_count_line("2026-05-01T10:00:01.000Z", Some(&[("input_tokens", 5), ("output_tokens", 5), ("cached_input_tokens", 0), ("reasoning_output_tokens", 0)]), None),
            // user 事件
            r#"{"timestamp":"2026-05-01T10:00:02.000Z","type":"event_msg","payload":{"type":"user_message"}}"#.to_string(),
        ];
        // 无 turn_context：模型 unknown
        let events = settle_new_lines(&lines, &mut model, &mut totals);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].model, "unknown");
        assert_eq!(events[0].tokens, 10);

        // 中途 turn_context 换模型：后续事件用新模型
        let lines2 = vec![
            turn_context_line("glm-4.6"),
            token_count_line(
                "2026-05-01T10:01:00.000Z",
                Some(&[
                    ("input_tokens", 7),
                    ("output_tokens", 1),
                    ("cached_input_tokens", 0),
                    ("reasoning_output_tokens", 0),
                ]),
                None,
            ),
        ];
        let events2 = settle_new_lines(&lines2, &mut model, &mut totals);
        assert_eq!(events2.len(), 1);
        assert_eq!(events2[0].model, "glm-4.6");
    }

    #[test]
    fn auth_keys_reads_json_and_bearer_token() {
        let dir = std::env::temp_dir().join(format!("kcb-codex-auth-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(auth_keys(&dir).is_empty()); // 两个文件都不存在

        std::fs::write(
            dir.join("auth.json"),
            r#"{"OPENAI_API_KEY":"sk-openai-key","tokens":{"id_token":"x"}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("config.toml"),
            r#"
model_provider = "kimi"
[model_providers.kimi]
name = "Kimi"
experimental_bearer_token = "sk-kimi-via-codex"
"#,
        )
        .unwrap();
        let keys = auth_keys(&dir);
        assert_eq!(
            keys,
            vec!["sk-openai-key".to_string(), "sk-kimi-via-codex".to_string()]
        );

        // 两个来源同 key：去重
        std::fs::write(
            dir.join("auth.json"),
            r#"{"OPENAI_API_KEY":"sk-kimi-via-codex"}"#,
        )
        .unwrap();
        assert_eq!(auth_keys(&dir), vec!["sk-kimi-via-codex".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_files_walks_sessions_and_archived() {
        let dir = std::env::temp_dir().join(format!("kcb-codex-files-{}", uuid::Uuid::new_v4()));
        let day = dir.join("sessions").join("2026").join("05").join("01");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::create_dir_all(dir.join("archived_sessions")).unwrap();
        std::fs::write(day.join("rollout-abc.jsonl"), "{}").unwrap();
        std::fs::write(
            dir.join("archived_sessions").join("rollout-old.jsonl"),
            "{}",
        )
        .unwrap();
        // 非 jsonl 不收；config 等顶层文件不收
        std::fs::write(dir.join("config.toml"), "").unwrap();
        std::fs::write(day.join("rollout.xyz"), "").unwrap();

        let files = collect_files(&dir);
        // 排序按全路径字典序：archived_sessions 前于 sessions
        let names: Vec<&str> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(names, vec!["rollout-old.jsonl", "rollout-abc.jsonl"]);
        assert!(collect_files(&dir.join("nonexistent")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
