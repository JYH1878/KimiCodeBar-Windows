//! Claude Code 本地日志（`~/.claude/projects/**/*.jsonl`）解析与归属快照。
//! 由 local_usage.rs 的扫描编排调用；本文件只含纯函数与文件读取，状态归父模块。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{ClaudeIdEntry, UsageEvent};

/// 归属 key：`~/.claude/settings.json` 的 `env.ANTHROPIC_AUTH_TOKEN`（空白按未配置）。
/// OAuth 形态的 Claude 登录没有该字段，自然落未归属（设计内）
pub(super) fn auth_token(claude_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(claude_dir.join("settings.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value
        .get("env")?
        .get("ANTHROPIC_AUTH_TOKEN")?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// 递归收集 `projects/` 下全部 .jsonl（含 session 目录里的 subagents/ 与
/// workflows/ 嵌套层级，journal.jsonl 等无 assistant 行的文件解析时天然跳过），
/// 排序保证处理顺序确定（状态落盘内容可复现）
pub(super) fn collect_files(claude_dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_recursive(&claude_dir.join("projects"), &mut files);
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

/// 一条 assistant 消息的解析结果（去重键 = message.id）
#[derive(Debug, PartialEq)]
struct ClaudeMsg {
    id: String,
    ts_ms: i64,
    model: String,
    tokens: u64,
}

/// 解析单行：只认 `"type":"assistant"` 行，tokens = usage 四字段之和
/// （input_tokens + output_tokens + cache_read_input_tokens +
/// cache_creation_input_tokens），时间 = 顶层 timestamp（RFC3339）。
/// 缺 message.id / 缺 timestamp / 坏 JSON 返回 None（无法定位去重键或日期）；
/// 缺 model 计 "unknown" 桶、缺 usage 按 0（与 Kimi 解析同策略）
fn parse_line(line: &str) -> Option<ClaudeMsg> {
    #[derive(serde::Deserialize)]
    struct Line {
        #[serde(rename = "type")]
        kind: String,
        #[serde(default)]
        message: Option<Message>,
        #[serde(default)]
        timestamp: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct Message {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        usage: Option<Usage>,
    }
    #[derive(Default, serde::Deserialize)]
    struct Usage {
        #[serde(default)]
        input_tokens: u64,
        #[serde(default)]
        output_tokens: u64,
        #[serde(default)]
        cache_read_input_tokens: u64,
        #[serde(default)]
        cache_creation_input_tokens: u64,
    }

    let line: Line = serde_json::from_str(line).ok()?;
    if line.kind != "assistant" {
        return None;
    }
    let message = line.message?;
    let id = message.id?.trim().to_string();
    if id.is_empty() {
        return None;
    }
    let ts_ms = chrono::DateTime::parse_from_rfc3339(line.timestamp?.trim())
        .ok()?
        .timestamp_millis();
    let usage = message.usage.unwrap_or_default();
    Some(ClaudeMsg {
        id,
        ts_ms,
        model: message.model.unwrap_or_else(|| "unknown".to_string()),
        tokens: usage.input_tokens
            + usage.output_tokens
            + usage.cache_read_input_tokens
            + usage.cache_creation_input_tokens,
    })
}

/// 本批新行按 message.id 去重出账（流式会把同一 id 写多次，取 tokens 最大者作代表），
/// 再与跨扫描已计入值（ids，全局去重：resume 会话文件会复制旧消息）差分：
/// 首见全计，补写只在变大时出差额，变小忽略。seen_ms 供 48h 裁剪。
/// 返回事件按时间升序（状态落盘可复现）
pub(super) fn settle_new_lines(
    lines: &[String],
    ids: &mut HashMap<String, ClaudeIdEntry>,
) -> Vec<UsageEvent> {
    // 本批内先归并：同 id 只留 tokens 最大的一行
    let mut best: HashMap<String, ClaudeMsg> = HashMap::new();
    for line in lines {
        let Some(msg) = parse_line(line) else {
            continue;
        };
        let replace = best
            .get(&msg.id)
            .map_or(true, |old| msg.tokens > old.tokens);
        if replace {
            best.insert(msg.id.clone(), msg);
        }
    }
    let mut events = Vec::new();
    for (id, msg) in best {
        let counted = ids.get(&id).map_or(0, |entry| entry.tokens);
        if msg.tokens > counted {
            events.push(UsageEvent {
                ts_ms: msg.ts_ms,
                model: msg.model.clone(),
                tokens: msg.tokens - counted,
            });
        }
        ids.insert(
            id,
            ClaudeIdEntry {
                tokens: msg.tokens.max(counted),
                seen_ms: msg.ts_ms,
            },
        );
    }
    events.sort_by_key(|event| event.ts_ms);
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实格式 assistant 行（字段组合与 cc-switch fixture 同构）
    fn assistant_line(id: &str, model: &str, tokens: u64, rfc3339: &str) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"id":"{id}","model":"{model}","usage":{{"input_tokens":3,"output_tokens":{tokens},"cache_read_input_tokens":5000,"cache_creation_input_tokens":10000}},"stop_reason":"end_turn"}},"timestamp":"{rfc3339}","sessionId":"session-abc"}}"#
        )
    }

    #[test]
    fn parse_line_reads_real_assistant_fixture() {
        let line = r#"{"type":"assistant","message":{"id":"msg_test123","model":"claude-opus-4-6","usage":{"input_tokens":3,"output_tokens":150,"cache_read_input_tokens":5000,"cache_creation_input_tokens":10000},"stop_reason":"end_turn"},"timestamp":"2026-04-05T12:00:00Z","sessionId":"session-abc"}"#;
        let msg = parse_line(line).expect("真实格式应能解析");
        assert_eq!(msg.id, "msg_test123");
        assert_eq!(msg.model, "claude-opus-4-6");
        // tokens = 3 + 150 + 5000 + 10000
        assert_eq!(msg.tokens, 15153);
        assert_eq!(
            msg.ts_ms,
            chrono::DateTime::parse_from_rfc3339("2026-04-05T12:00:00Z")
                .unwrap()
                .timestamp_millis()
        );
    }

    #[test]
    fn parse_line_skips_non_assistant_and_degenerate_lines() {
        // user / 汇总行不是消耗
        assert!(parse_line(
            r#"{"type":"user","message":{"role":"user","content":"hi"},"timestamp":"2026-04-05T12:00:00Z"}"#
        )
        .is_none());
        assert!(parse_line(r#"{"type":"summary","summary":"s"}"#).is_none());
        // 缺 message.id：无去重键
        assert!(parse_line(
            r#"{"type":"assistant","message":{"model":"m","usage":{"input_tokens":1}},"timestamp":"2026-04-05T12:00:00Z"}"#
        )
        .is_none());
        // 缺 timestamp：无法定位日期
        assert!(parse_line(
            r#"{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":1}}}"#
        )
        .is_none());
        // 坏 JSON
        assert!(parse_line("not json").is_none());
        // 缺 model → unknown；缺 usage → 0 tokens（占位事件仍有活跃意义）
        let msg = parse_line(
            r#"{"type":"assistant","message":{"id":"m2"},"timestamp":"2026-04-05T12:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(msg.model, "unknown");
        assert_eq!(msg.tokens, 0);
    }

    #[test]
    fn settle_dedups_by_id_takes_max_and_diffs_across_batches() {
        let mut ids = HashMap::new();
        // 同一 id 流式重复写三次（100 → 50 → 150）：取最大 150，只出一条
        let batch = [
            assistant_line("msg_1", "claude-opus-4-6", 100, "2026-04-05T12:00:00Z"),
            assistant_line("msg_1", "claude-opus-4-6", 50, "2026-04-05T12:00:01Z"),
            assistant_line("msg_1", "claude-opus-4-6", 150, "2026-04-05T12:00:02Z"),
        ]
        .map(String::from);
        let events = settle_new_lines(&batch, &mut ids);
        assert_eq!(events.len(), 1);
        // tokens = 3 + 150 + 5000 + 10000
        assert_eq!(events[0].tokens, 15153);
        assert_eq!(events[0].model, "claude-opus-4-6");

        // 补写同 id 更大值（200）：只出差额（200-150=50，加上不变的四字段？不——
        // tokens 是四字段之和，input 3 / cache 5000 / cache_creation 10000 恒定，
        // 差额 = (3+200+5000+10000) - (3+150+5000+10000) = 50）
        let batch2 = [assistant_line(
            "msg_1",
            "claude-opus-4-6",
            200,
            "2026-04-05T12:03:00Z",
        )]
        .map(String::from);
        let events2 = settle_new_lines(&batch2, &mut ids);
        assert_eq!(events2.len(), 1);
        assert_eq!(events2[0].tokens, 50);
        assert_eq!(
            events2[0].ts_ms,
            chrono::DateTime::parse_from_rfc3339("2026-04-05T12:03:00Z")
                .unwrap()
                .timestamp_millis()
        );

        // 变小（120）：忽略，不出事件
        let batch3 = [assistant_line(
            "msg_1",
            "claude-opus-4-6",
            120,
            "2026-04-05T12:04:00Z",
        )]
        .map(String::from);
        assert!(settle_new_lines(&batch3, &mut ids).is_empty());

        // 新 id 全额入账；多事件按时间升序
        let batch4 = [
            assistant_line("msg_b", "claude-opus-4-6", 10, "2026-04-05T13:00:00Z"),
            assistant_line("msg_a", "claude-opus-4-6", 20, "2026-04-05T12:00:00Z"),
        ]
        .map(String::from);
        let events4 = settle_new_lines(&batch4, &mut ids);
        assert_eq!(events4.len(), 2);
        assert!(events4[0].ts_ms < events4[1].ts_ms);
    }

    #[test]
    fn auth_token_reads_settings_env() {
        let dir = std::env::temp_dir().join(format!("kcb-claude-auth-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(auth_token(&dir), None); // settings.json 不存在

        std::fs::write(
            dir.join("settings.json"),
            r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"sk-ant-token"},"model":"opus"}"#,
        )
        .unwrap();
        assert_eq!(auth_token(&dir).as_deref(), Some("sk-ant-token"));

        // 空白按未配置
        std::fs::write(
            dir.join("settings.json"),
            r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"  "}}"#,
        )
        .unwrap();
        assert_eq!(auth_token(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_files_walks_projects_recursively() {
        let dir = std::env::temp_dir().join(format!("kcb-claude-files-{}", uuid::Uuid::new_v4()));
        let projects = dir.join("projects");
        let subagents = projects.join("proj-a").join("ses-1").join("subagents");
        let workflows = subagents.join("workflows").join("wf_x");
        std::fs::create_dir_all(&workflows).unwrap();
        std::fs::create_dir_all(projects.join("proj-b")).unwrap();
        std::fs::write(projects.join("proj-b").join("main.jsonl"), "{}").unwrap();
        std::fs::write(subagents.join("agent-1.jsonl"), "{}").unwrap();
        std::fs::write(workflows.join("agent-wf.jsonl"), "{}").unwrap();
        // 非 jsonl 与 projects 外的文件不收
        std::fs::write(projects.join("proj-b").join("notes.txt"), "").unwrap();
        std::fs::write(dir.join("settings.json"), "{}").unwrap();

        let files = collect_files(&dir);
        let names: Vec<&str> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(names, vec!["agent-1.jsonl", "agent-wf.jsonl", "main.jsonl"]);
        // 目录不存在容忍为空
        assert!(collect_files(&dir.join("nonexistent")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
