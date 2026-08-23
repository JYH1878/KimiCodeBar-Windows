//! OpenCode 本地日志（opencode.db，SQLite 只读）解析与归属快照。
//! 由 local_usage.rs 的扫描编排调用；本文件只含纯函数与数据库读取，状态归父模块。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{OpenCodeDbState, UsageEvent};

/// 水位回看的重叠窗：消息可能乱序落库/在写入事务里迟到，回看 5 分钟 + id 去重兜底
const OVERLAP_MS: i64 = 5 * 60 * 1000;

/// providerID → key 快照：候选配置目录逐个读，同 id 先到的来源优先。
/// - `auth.json`：`{"<providerID>": {"type":"api","key":"..."}}`，仅认 type=="api"
///   且 key 非空（OAuth 形态自然没有 key，落未归属）；
/// - `opencode.json` 的 `provider.<id>.options.apiKey` 字面值补充，`${...}` 模板跳过
pub(super) fn provider_keys(config_dirs: &[PathBuf]) -> HashMap<String, String> {
    let mut map: HashMap<String, String> = HashMap::new();
    for dir in config_dirs {
        if let Ok(text) = std::fs::read_to_string(dir.join("auth.json")) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(entries) = value.as_object() {
                    for (id, entry) in entries {
                        let is_api = entry.get("type").and_then(|v| v.as_str()) == Some("api");
                        let key = entry
                            .get("key")
                            .and_then(|v| v.as_str())
                            .map(str::trim)
                            .filter(|s| !s.is_empty());
                        if is_api {
                            if let Some(key) = key {
                                map.entry(id.clone()).or_insert_with(|| key.to_string());
                            }
                        }
                    }
                }
            }
        }
        if let Ok(text) = std::fs::read_to_string(dir.join("opencode.json")) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(providers) = value.get("provider").and_then(|v| v.as_object()) {
                    for (id, def) in providers {
                        let Some(api_key) = def
                            .get("options")
                            .and_then(|o| o.get("apiKey"))
                            .and_then(|v| v.as_str())
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                        else {
                            continue;
                        };
                        // 模板引用（${ENV_VAR}）解析不了环境变量，跳过
                        if api_key.starts_with("${") {
                            continue;
                        }
                        map.entry(id.clone()).or_insert_with(|| api_key.to_string());
                    }
                }
            }
        }
    }
    map
}

/// 扫单个 opencode.db（只读打开 + PRAGMA query_only=ON 双保险）：
/// 查 `message` 表 `time_created >= 水位 - 5min`（升序），只计
/// `data.role=="assistant"` 且有 `time.completed` 的行；tokens =
/// `tokens.input+output+reasoning+cache.read+cache.write`；时间 = `time_created`
/// 列（epoch 毫秒）；模型 = `data.modelID`（缺省 unknown）。
/// 已计 id 进去重集（水位重叠窗内重复行不双计），水位推到本批最大 time_created。
/// 任何失败（表不存在/库损坏/占用）容忍为空——下次扫描重试
pub(super) fn scan_db(
    db_path: &Path,
    state: &mut OpenCodeDbState,
) -> Vec<(UsageEvent, Option<String>)> {
    let mut out = Vec::new();
    let Ok(conn) =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return out;
    };
    let _ = conn.execute_batch("PRAGMA query_only = ON;");
    let Ok(mut stmt) =
        conn.prepare("SELECT id, time_created, data FROM message WHERE time_created >= ?1 ORDER BY time_created ASC, id ASC")
    else {
        return out;
    };
    let since = state.watermark_ms.saturating_sub(OVERLAP_MS);
    let Ok(rows) = stmt.query_map([since], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
        ))
    }) else {
        return out;
    };

    let mut watermark = state.watermark_ms;
    for row in rows.flatten() {
        let (id, time_created, data) = row;
        if time_created > watermark {
            watermark = time_created;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&data) else {
            continue;
        };
        if value.get("role").and_then(|v| v.as_str()) != Some("assistant") {
            continue;
        }
        // 只有完成的消息才计（time.completed 存在且 > 0）
        let completed = value
            .get("time")
            .and_then(|t| t.get("completed"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        if completed <= 0.0 {
            continue;
        }
        if state.ids.contains_key(&id) {
            continue;
        }
        let tokens_of = |name: &str| {
            value
                .get("tokens")
                .and_then(|t| t.get(name))
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
        };
        let cache_read = value
            .get("tokens")
            .and_then(|t| t.get("cache"))
            .and_then(|c| c.get("read"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cache_write = value
            .get("tokens")
            .and_then(|t| t.get("cache"))
            .and_then(|c| c.get("write"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let provider = value
            .get("providerID")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        state.ids.insert(id, time_created);
        out.push((
            UsageEvent {
                ts_ms: time_created,
                model: value
                    .get("modelID")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                tokens: tokens_of("input")
                    + tokens_of("output")
                    + tokens_of("reasoning")
                    + cache_read
                    + cache_write,
            },
            provider,
        ));
    }
    state.watermark_ms = watermark;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 建一个真实 schema 的 opencode.db 并插行（data 为 message 表 JSON 原文）
    fn build_db(dir: &Path, rows: &[(i64, &str)]) -> PathBuf {
        let db = dir.join("opencode.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "CREATE TABLE message (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL,
                time_updated INTEGER,
                data TEXT NOT NULL
            )",
            [],
        )
        .unwrap();
        for (idx, (ts, data)) in rows.iter().enumerate() {
            conn.execute(
                "INSERT INTO message (id, session_id, time_created, time_updated, data)
                 VALUES (?1, ?2, ?3, ?3, ?4)",
                rusqlite::params![format!("msg_{idx}"), "ses_1", ts, data],
            )
            .unwrap();
        }
        db
    }

    fn assistant_data(model: &str, provider: &str, tokens: u64) -> String {
        serde_json::json!({
            "role": "assistant",
            "providerID": provider,
            "modelID": model,
            "tokens": {"input": tokens - 10, "output": 5, "reasoning": 3, "cache": {"read": 1, "write": 1}},
            "time": {"created": 1, "completed": 2},
        })
        .to_string()
    }

    #[test]
    fn scan_db_counts_completed_assistant_rows() {
        let dir = std::env::temp_dir().join(format!("kcb-oc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let ts = 1_800_000_000_000i64;
        let db = build_db(
            &dir,
            &[
                (ts, &assistant_data("kimi-k3", "kimi", 100)), // tokens = 90+5+3+1+1 = 100
                (ts + 1, r#"{"role":"user","tokens":{"input":1}}"#), // 非助理：跳过
                (ts + 2, r#"{"role":"assistant","time":{"created":1}}"#), // 未完成：跳过
            ],
        );
        let mut state = OpenCodeDbState::default();
        let events = scan_db(&db, &mut state);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0.tokens, 100);
        assert_eq!(events[0].0.model, "kimi-k3");
        assert_eq!(events[0].0.ts_ms, ts);
        assert_eq!(events[0].1.as_deref(), Some("kimi"));
        assert_eq!(state.watermark_ms, ts + 2);
        assert_eq!(state.ids.len(), 1); // 只有计入的进去重集

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_db_watermark_and_dedup_across_scans() {
        let dir = std::env::temp_dir().join(format!("kcb-oc2-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let base = 1_800_000_000_000i64;
        let db = build_db(&dir, &[(base, &assistant_data("kimi-k3", "kimi", 50))]);
        let mut state = OpenCodeDbState::default();
        let first = scan_db(&db, &mut state);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].0.tokens, 50);

        // 无新行：二次扫描为空（水位之后无行）
        assert!(scan_db(&db, &mut state).is_empty());

        // 追加一条更新的消息（同库）：只增量这一条
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute(
                "INSERT INTO message (id, session_id, time_created, time_updated, data)
                 VALUES ('msg_new', 'ses_1', ?1, ?1, ?2)",
                rusqlite::params![base + 60_000, assistant_data("glm-4.6", "glm", 30)],
            )
            .unwrap();
        }
        let second = scan_db(&db, &mut state);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].0.model, "glm-4.6");
        assert_eq!(second[0].1.as_deref(), Some("glm"));

        // 在重叠窗内（水位 - 5min）补一条更早时间戳的新消息：水位回看能捡到，id 去重不影响新 id
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute(
                "INSERT INTO message (id, session_id, time_created, time_updated, data)
                 VALUES ('msg_late', 'ses_1', ?1, ?1, ?2)",
                rusqlite::params![base + 10_000, assistant_data("kimi-k3", "kimi", 20)],
            )
            .unwrap();
        }
        let third = scan_db(&db, &mut state);
        assert_eq!(third.len(), 1);
        assert_eq!(third[0].0.tokens, 20);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_db_tolerates_missing_table_and_garbage() {
        let dir = std::env::temp_dir().join(format!("kcb-oc3-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // 无 message 表的空库：空结果
        let db = dir.join("opencode.db");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute("CREATE TABLE other (x)", []).unwrap();
        }
        let mut state = OpenCodeDbState::default();
        assert!(scan_db(&db, &mut state).is_empty());
        // 坏 JSON 的 data 行容忍跳过
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute(
                "CREATE TABLE message (
                    id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                    time_created INTEGER NOT NULL, time_updated INTEGER, data TEXT NOT NULL
                )",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO message (id, session_id, time_created, time_updated, data)
                 VALUES ('m1', 's', 100, 100, 'not json')",
                [],
            )
            .unwrap();
        }
        assert!(scan_db(&db, &mut state).is_empty());
        // 文件不存在：空结果
        let mut state2 = OpenCodeDbState::default();
        assert!(scan_db(&dir.join("nope.db"), &mut state2).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn provider_keys_reads_auth_json_then_opencode_json() {
        let dir = std::env::temp_dir().join(format!("kcb-oc4-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("auth.json"),
            r#"{
                "kimi": {"type": "api", "key": "sk-kimi-oc"},
                "oauth-provider": {"type": "oauth", "key": "should-not-count"},
                "empty": {"type": "api", "key": "  "}
            }"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("opencode.json"),
            r#"{
                "provider": {
                    "kimi": {"options": {"apiKey": "sk-ignored-auth-wins"}},
                    "glm": {"options": {"apiKey": "sk-glm-literal"}},
                    "tmpl": {"options": {"apiKey": "${SOME_ENV}"}}
                }
            }"#,
        )
        .unwrap();

        let map = provider_keys(std::slice::from_ref(&dir));
        assert_eq!(map.get("kimi").map(String::as_str), Some("sk-kimi-oc"));
        assert_eq!(map.get("glm").map(String::as_str), Some("sk-glm-literal"));
        assert!(!map.contains_key("oauth-provider"));
        assert!(!map.contains_key("empty"));
        assert!(!map.contains_key("tmpl"));

        // 目录为空 / 文件缺失：空 map
        let empty_dir = std::env::temp_dir().join(format!("kcb-oc5-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&empty_dir).unwrap();
        assert!(provider_keys(std::slice::from_ref(&empty_dir)).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&empty_dir);
    }
}
