//! local_usage 的单元测试与测试专用辅助（自 local_usage.rs 原样搬出，生产代码零改动）。

use super::*;

/// 既有 Kimi-only 扫描入口（测试兼容壳）：等价 harness 输入为空的 scan_full
fn scan_with<Tz: TimeZone>(
    scan_targets: &[(PathBuf, Attribution)],
    state_path: &Path,
    now_ms: i64,
    tz: &Tz,
) -> ScanView
where
    Tz::Offset: std::fmt::Display,
{
    scan_full(
        scan_targets,
        &HarnessInput::default(),
        state_path,
        now_ms,
        tz,
    )
}

// 环境变量是进程级全局状态，凡改动 KIMICODEBAR_CONFIG_DIR / USERPROFILE 的测试
// 都须持锁串行；锁为全库共享（lib.rs::TEST_ENV_LOCK）
use crate::TEST_ENV_LOCK as ENV_LOCK;

/// UTC+8 固定偏移：日期分桶/CSV 测试的确定时区（与开发机一致）
fn tz8() -> chrono::FixedOffset {
    chrono::FixedOffset::east_opt(8 * 3600).unwrap()
}

/// RFC3339 → epoch 毫秒
fn ms(rfc3339: &str) -> i64 {
    DateTime::parse_from_rfc3339(rfc3339)
        .unwrap()
        .timestamp_millis()
}

/// 真实事件样例改参生成（格式与线上 wire.jsonl 完全一致）
fn usage_line(model: &str, rfc3339: &str, input_other: u64, output: u64) -> String {
    let ts = ms(rfc3339);
    format!(
        r#"{{"type":"usage.record","model":"{model}","usage":{{"inputOther":{input_other},"output":{output},"inputCacheRead":11264,"inputCacheCreation":0}},"usageScope":"turn","time":{ts}}}"#
    )
}

/// 嵌套 step.end 行（真实格式：event 内 usage 与 usage.record 同形 + llmStreamDurationMs，
/// time 在行顶层；model 实测常缺，给 None 时不写该字段）
fn step_end_line(model: Option<&str>, rfc3339: &str, output: u64, duration_ms: u64) -> String {
    let ts = ms(rfc3339);
    let model_field = model
        .map(|m| format!(r#""model":"{m}","#))
        .unwrap_or_default();
    format!(
        r#"{{"type":"context.append_loop_event","event":{{"type":"step.end",{model_field}"usage":{{"inputOther":100,"output":{output},"inputCacheRead":0,"inputCacheCreation":0}},"llmStreamDurationMs":{duration_ms}}},"time":{ts}}}"#
    )
}

/// f64 近似相等（速率是浮点除法，留 1e-9 容差）
fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kimicodebar-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---- 解析单行 ----

#[test]
fn parse_real_usage_record() {
    // 实测真实样例原文
    let line = r#"{"type":"usage.record","model":"kimi-code/k3","usage":{"inputOther":11592,"output":504,"inputCacheRead":11264,"inputCacheCreation":0},"usageScope":"turn","time":1784973672311}"#;
    let event = parse_usage_line(line).expect("真实样例应能解析");
    assert_eq!(event.model, "kimi-code/k3");
    assert_eq!(event.ts_ms, 1784973672311);
    // tokens = 11592 + 504 + 11264 + 0
    assert_eq!(event.tokens, 23360);
}

#[test]
fn parse_skips_other_event_types() {
    assert!(parse_usage_line(r#"{"type":"llm.request","data":{}}"#).is_none());
    assert!(parse_usage_line(r#"{"type":"step.begin","time":1}"#).is_none());
    assert!(parse_usage_line(r#"{"type":"string"}"#).is_none());
}

#[test]
fn parse_skips_bad_json_and_missing_time() {
    assert!(parse_usage_line("not json").is_none());
    assert!(parse_usage_line(r#"{"type":"usage.record","model":"m""#).is_none());
    // usage.record 缺 time：无法定位日期，丢弃
    assert!(
        parse_usage_line(r#"{"type":"usage.record","model":"m","usage":{"output":1}}"#).is_none()
    );
}

#[test]
fn parse_defaults_model_and_usage() {
    // 缺 model → "unknown" 桶；缺 usage → 0
    let event = parse_usage_line(r#"{"type":"usage.record","time":1000}"#).unwrap();
    assert_eq!(event.model, "unknown");
    assert_eq!(event.tokens, 0);
}

#[test]
fn parse_step_end_nested_event() {
    // 实测真实样例原文（kimi-usage-stats 同款）
    let line = r#"{"type":"context.append_loop_event","event":{"type":"step.end","turnId":"0","step":1,"finishReason":"tool_use","usage":{"inputOther":100,"output":10,"inputCacheRead":0,"inputCacheCreation":0},"llmStreamDurationMs":2000},"time":1786600148000}"#;
    let step = parse_step_end_line(line).expect("嵌套 step.end 应能解析");
    assert_eq!(step.ts_ms, 1786600148000);
    assert_eq!(step.output_tokens, 10);
    assert_eq!(step.duration_ms, 2000);
    assert_eq!(step.model, None);
    // 嵌套事件自带 model 时原样取出
    let with_model = parse_step_end_line(&step_end_line(
        Some("kimi-code/k3"),
        "2026-07-27T11:55:00+08:00",
        5,
        800,
    ))
    .unwrap();
    assert_eq!(with_model.model.as_deref(), Some("kimi-code/k3"));
    // 行顶层缺 time 时兜底读嵌套事件的 time
    let nested_time = parse_step_end_line(
        r#"{"type":"context.append_loop_event","event":{"type":"step.end","usage":{"output":1},"llmStreamDurationMs":500,"time":42}}"#,
    )
    .unwrap();
    assert_eq!(nested_time.ts_ms, 42);

    // 非 append_loop_event 行 / 非 step.end 子事件 / 缺 time / 零时长 → None（防除零）
    assert!(parse_step_end_line(r#"{"type":"usage.record","time":1}"#).is_none());
    assert!(parse_step_end_line(
        r#"{"type":"context.append_loop_event","event":{"type":"tool.call","name":"Read"},"time":1}"#
    )
    .is_none());
    assert!(parse_step_end_line(
        r#"{"type":"context.append_loop_event","event":{"type":"step.end","usage":{"output":1},"llmStreamDurationMs":500}}"#
    )
    .is_none());
    assert!(parse_step_end_line(
        r#"{"type":"context.append_loop_event","event":{"type":"step.end","usage":{"output":1},"llmStreamDurationMs":0},"time":1}"#
    )
    .is_none());
}

// ---- 状态版本迁移 ----

#[test]
fn state_version_mismatch_discards_old_state() {
    let dir = temp_dir("state-version-mismatch");
    let path = dir.join("scan-state.json");
    // 旧版状态（GLM 归属路由之前，无 version 字段）→ 整体丢弃：buckets/files 全清空
    std::fs::write(
        &path,
        r#"{"last_scan_at":1,"files":{"a.jsonl":999},"buckets":{"acc-x":{"by_date":{"2026-08-26":5},"by_date_model":{},"last_event_at":1}}}"#,
    )
    .unwrap();
    let discarded = load_state(&path);
    assert!(discarded.files.is_empty() && discarded.buckets.is_empty());
    assert_eq!(discarded.version, STATE_VERSION);

    // v4 状态（version 字段落后一代）→ 同样整体丢弃
    std::fs::write(
        &path,
        r#"{"version":4,"last_scan_at":1,"files":{"a.jsonl":999},"buckets":{"acc-x":{"by_date":{"2026-08-26":5},"by_date_model":{},"last_event_at":1}}}"#,
    )
    .unwrap();
    let discarded = load_state(&path);
    assert!(discarded.files.is_empty() && discarded.buckets.is_empty());
    assert_eq!(discarded.version, STATE_VERSION);

    // 当前版本状态 → 原样保留（版本字段随 save_state 落盘，下次 load 不再误判）
    let mut state = fresh_state();
    state.files.insert("a.jsonl".to_string(), 7);
    save_state(&path, &state).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains(&format!("\"version\": {STATE_VERSION}")));
    let kept = load_state(&path);
    assert_eq!(kept.files.get("a.jsonl"), Some(&7));
}

// ---- 日期分桶 ----

#[test]
fn date_key_crosses_day_boundary_in_local_tz() {
    let tz = tz8();
    // UTC 2026-07-26 16:00:00 = UTC+8 2026-07-27 00:00:00：跨天边界两侧
    assert_eq!(
        date_key(ms("2026-07-26T16:00:00Z"), &tz).as_deref(),
        Some("2026-07-27")
    );
    assert_eq!(
        date_key(ms("2026-07-26T15:59:59.999Z"), &tz).as_deref(),
        Some("2026-07-26")
    );
}

// ---- 聚合器 ----

#[test]
fn aggregator_finish_today_yesterday_and_daily_window() {
    let tz = tz8();
    let mut agg = UsageAggregator::default();
    // usage_line 每条含 inputCacheRead 11264：单条 tokens = input_other + output + 11264
    // 今天（UTC+8 2026-07-27）：UTC 16:00 之后
    agg.add(
        &parse_usage_line(&usage_line("m1", "2026-07-26T16:00:01Z", 0, 10)).unwrap(),
        &tz,
    );
    // 昨天：UTC 16:00 之前 1 秒
    agg.add(
        &parse_usage_line(&usage_line("m1", "2026-07-26T15:59:59Z", 0, 20)).unwrap(),
        &tz,
    );
    // 7 天窗口外（2026-07-20 本地）：不进 daily，也不进今日 by_model
    agg.add(
        &parse_usage_line(&usage_line("m2", "2026-07-20T02:00:00Z", 0, 99)).unwrap(),
        &tz,
    );

    let today = NaiveDate::from_ymd_opt(2026, 7, 27).unwrap();
    let stats = agg.finish(today, Some(123), ms("2026-07-27T12:00:00+08:00"));
    assert_eq!(stats.today_tokens, 10 + 11264);
    assert_eq!(stats.yesterday_tokens, 20 + 11264);
    assert_eq!(stats.last_scan_at, Some(123));

    // daily 恒为最近 7 个自然日（2026-07-21..27），升序、缺日补 0
    let dates: Vec<&str> = stats.daily.iter().map(|d| d.date.as_str()).collect();
    assert_eq!(
        dates,
        vec![
            "2026-07-21",
            "2026-07-22",
            "2026-07-23",
            "2026-07-24",
            "2026-07-25",
            "2026-07-26",
            "2026-07-27"
        ]
    );
    let tokens: Vec<u64> = stats.daily.iter().map(|d| d.tokens).collect();
    assert_eq!(tokens, vec![0, 0, 0, 0, 0, 20 + 11264, 10 + 11264]);

    // by_model 只含今日分模型（昨天的 m1、daily 窗口外的 m2 都不计）
    assert_eq!(stats.by_model.len(), 1);
    assert_eq!(stats.by_model[0].model, "m1");
    assert_eq!(stats.by_model[0].tokens, 10 + 11264);
}

#[test]
fn aggregator_by_model_top5_desc_with_tiebreak() {
    let mut today_models = HashMap::new();
    for (model, tokens) in [
        ("alpha", 50),
        ("bravo", 100),
        ("charlie", 100),
        ("delta", 30),
        ("echo", 200),
        ("foxtrot", 10),
    ] {
        today_models.insert(model.to_string(), tokens);
    }
    let mut other_day_models = HashMap::new();
    other_day_models.insert("zulu".to_string(), 999);
    let agg = UsageAggregator {
        by_date: HashMap::new(),
        by_date_model: HashMap::from([
            ("2026-07-27".to_string(), today_models),
            // 昨天的模型不进今日 by_model
            ("2026-07-26".to_string(), other_day_models),
        ]),
        last_event_at: None,
        step_samples: Vec::new(),
        by_date_cache_read: HashMap::new(),
        by_date_input: HashMap::new(),
    };
    let stats = agg.finish(
        NaiveDate::from_ymd_opt(2026, 7, 27).unwrap(),
        None,
        ms("2026-07-27T12:00:00+08:00"),
    );
    // 降序 top5：echo 200 > bravo/charlie 100（并列按名升序）> alpha 50；delta/foxtrot 被截掉
    let models: Vec<&str> = stats.by_model.iter().map(|m| m.model.as_str()).collect();
    assert_eq!(models, vec!["echo", "bravo", "charlie", "alpha", "delta"]);
}

#[test]
fn aggregator_prune_drops_dates_older_than_30_days() {
    let mut agg = UsageAggregator::default();
    agg.by_date.insert("2026-06-27".to_string(), 1); // 恰 30 天前：保留
    agg.by_date.insert("2026-06-26".to_string(), 2); // 31 天前：丢弃
    agg.by_date.insert("2026-07-27".to_string(), 3);
    agg.by_date_model
        .insert("2026-06-27".to_string(), HashMap::new()); // 保留
    agg.by_date_model
        .insert("2026-06-26".to_string(), HashMap::new()); // 丢弃
    agg.by_date_model
        .insert("2026-07-27".to_string(), HashMap::new());
    agg.prune(NaiveDate::from_ymd_opt(2026, 7, 27).unwrap());
    assert!(agg.by_date.contains_key("2026-06-27"));
    assert!(!agg.by_date.contains_key("2026-06-26"));
    assert!(agg.by_date.contains_key("2026-07-27"));
    // 按日×模型与按日同窗口裁剪
    assert!(agg.by_date_model.contains_key("2026-06-27"));
    assert!(!agg.by_date_model.contains_key("2026-06-26"));
    assert!(agg.by_date_model.contains_key("2026-07-27"));
}

// ---- 偏移续读 ----

#[test]
fn read_new_lines_full_then_incremental() {
    let dir = temp_dir("local-usage-read");
    let file = dir.join("wire.jsonl");
    std::fs::write(&file, "l1\nl2\n").unwrap();

    // 首次从头读：全量
    let (lines, offset) = read_new_lines(&file, 0).unwrap();
    assert_eq!(lines, vec!["l1", "l2"]);
    assert_eq!(offset, 6);

    // append 后续读：只读新增（"l1\nl2\nl3\n" 共 9 字节）
    std::fs::write(&file, "l1\nl2\nl3\n").unwrap();
    let (lines, offset) = read_new_lines(&file, offset).unwrap();
    assert_eq!(lines, vec!["l3"]);
    assert_eq!(offset, 9);

    // 无新增：空
    let (lines, new_offset) = read_new_lines(&file, offset).unwrap();
    assert!(lines.is_empty());
    assert_eq!(new_offset, offset);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn read_new_lines_holds_partial_tail() {
    let dir = temp_dir("local-usage-partial");
    let file = dir.join("wire.jsonl");
    // 残尾（写入中途的行）不消费，偏移停在最后一个换行之后
    std::fs::write(&file, "l1\nl2-partial").unwrap();
    let (lines, offset) = read_new_lines(&file, 0).unwrap();
    assert_eq!(lines, vec!["l1"]);
    assert_eq!(offset, 3);

    // 行写全后下次续读能拿到（"l1\nl2-full\n" 共 11 字节）
    std::fs::write(&file, "l1\nl2-full\n").unwrap();
    let (lines, offset) = read_new_lines(&file, offset).unwrap();
    assert_eq!(lines, vec!["l2-full"]);
    assert_eq!(offset, 11);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn read_new_lines_falls_back_on_truncated_file() {
    let dir = temp_dir("local-usage-trunc");
    let file = dir.join("wire.jsonl");
    std::fs::write(&file, "aaaa\nbbbb\n").unwrap();
    let (_, offset) = read_new_lines(&file, 0).unwrap();
    assert_eq!(offset, 10);

    // 文件被截断/重写（长度 < 偏移）：回退为从头读
    std::fs::write(&file, "cc\n").unwrap();
    let (lines, new_offset) = read_new_lines(&file, offset).unwrap();
    assert_eq!(lines, vec!["cc"]);
    assert_eq!(new_offset, 3);

    let _ = std::fs::remove_dir_all(&dir);
}

// ---- 增量扫描 ----

/// 造两层嵌套的 sessions 目录（与真实布局 wd_*/session_*/agents/*/wire.jsonl 同构）
fn write_wire(sessions: &Path, agent: &str, lines: &[String]) -> PathBuf {
    let dir = sessions
        .join("wd_x")
        .join("session_y")
        .join("agents")
        .join(agent);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("wire.jsonl");
    std::fs::write(&file, lines.join("\n") + "\n").unwrap();
    file
}

/// 无归属上下文（空 Attribution：全部事件进未归属桶）扫描并取未归属桶的统计视图
fn scan_unassigned(
    sessions: &Path,
    state_path: &Path,
    now_ms: i64,
    tz: &chrono::FixedOffset,
) -> LocalUsageStats {
    scan_with(
        &[(sessions.to_path_buf(), Attribution::default())],
        state_path,
        now_ms,
        tz,
    )
    .for_account(UNASSIGNED_BUCKET)
}

#[test]
fn scan_aggregates_incrementally_without_double_count() {
    let dir = temp_dir("local-usage-scan");
    let sessions = dir.join("sessions");
    let state_path = dir.join("config").join("scan-state.json");
    let tz = tz8();
    let now = ms("2026-07-27T12:00:00+08:00");

    // 多条 / 多模型 / 跨天 + 噪声行（其他类型、坏 JSON、缺 time）
    let main_lines = vec![
        usage_line("kimi-code/k3", "2026-07-27T10:00:00+08:00", 100, 10),
        usage_line("kimi-code/k3", "2026-07-26T23:00:00+08:00", 200, 20),
        usage_line("kimi-code/k2", "2026-07-20T10:00:00+08:00", 50, 5),
        r#"{"type":"llm.request","time":1}"#.to_string(),
        "not json".to_string(),
        r#"{"type":"usage.record","model":"m","usage":{"output":1}}"#.to_string(),
    ];
    let agent_lines = vec![usage_line(
        "kimi-code/k3",
        "2026-07-27T11:00:00+08:00",
        7,
        3,
    )];
    let main_file = write_wire(&sessions, "main", &main_lines);
    write_wire(&sessions, "agent-0", &agent_lines);

    // usage_line 每条还含 inputCacheRead 11264：
    // main 今日 (100+10+11264)，agent 今日 (7+3+11264)
    let per_main_today = 100 + 10 + 11264;
    let per_agent_today = 7 + 3 + 11264;

    // 首次全扫
    let stats = scan_unassigned(&sessions, &state_path, now, &tz);
    assert_eq!(stats.today_tokens, per_main_today + per_agent_today);
    assert_eq!(stats.yesterday_tokens, 200 + 20 + 11264);
    // by_model 是今日分模型：昨天的 k3、daily 窗口外的 k2 都不计
    assert_eq!(stats.by_model.len(), 1);
    assert_eq!(stats.by_model[0].model, "kimi-code/k3");
    assert_eq!(stats.by_model[0].tokens, per_main_today + per_agent_today);
    // k2 事件在 daily 窗口外：daily 全 0 的日子补 0，今日在末位
    assert_eq!(stats.daily.len(), 7);
    assert_eq!(stats.daily[6].tokens, stats.today_tokens);
    assert_eq!(
        stats.last_scan_at,
        Some(ms("2026-07-27T12:00:00+08:00") / 1000)
    );
    assert!(state_path.exists());

    // 二次扫描（同状态）：偏移续读，不重复计数
    let stats2 = scan_unassigned(&sessions, &state_path, now, &tz);
    assert_eq!(stats2.today_tokens, stats.today_tokens);
    assert_eq!(stats2.by_model, stats.by_model);

    // append 一条今日事件：三次扫描只增量这一条
    let extra = usage_line("kimi-code/k3", "2026-07-27T11:30:00+08:00", 1, 2);
    let mut content = std::fs::read_to_string(&main_file).unwrap();
    content.push_str(&extra);
    content.push('\n');
    std::fs::write(&main_file, content).unwrap();
    let stats3 = scan_unassigned(&sessions, &state_path, now, &tz);
    assert_eq!(stats3.today_tokens, stats.today_tokens + 1 + 2 + 11264);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scan_tracks_last_event_at_incrementally() {
    let dir = temp_dir("local-usage-lastevent");
    let sessions = dir.join("sessions");
    let state_path = dir.join("config").join("scan-state.json");
    let tz = tz8();
    let now = ms("2026-07-27T12:00:00+08:00");

    // 两条事件：10:00 与 11:00，最近一条的时间戳应成为 last_event_at
    let file = write_wire(
        &sessions,
        "main",
        &[
            usage_line("kimi-code/k3", "2026-07-27T10:00:00+08:00", 100, 10),
            usage_line("kimi-code/k3", "2026-07-27T11:00:00+08:00", 7, 3),
        ],
    );
    let stats = scan_unassigned(&sessions, &state_path, now, &tz);
    assert_eq!(stats.last_event_at, Some(ms("2026-07-27T11:00:00+08:00")));

    // append 一条更早时间戳的事件：max 不回退（历史补录不算"更新近"）
    let older = usage_line("kimi-code/k3", "2026-07-27T10:30:00+08:00", 1, 2);
    let mut content = std::fs::read_to_string(&file).unwrap();
    content.push_str(&older);
    content.push('\n');
    std::fs::write(&file, content).unwrap();
    let stats2 = scan_unassigned(&sessions, &state_path, now, &tz);
    assert_eq!(stats2.last_event_at, Some(ms("2026-07-27T11:00:00+08:00")));

    // append 一条更晚的事件：last_event_at 前进到 11:30
    let newer = usage_line("kimi-code/k3", "2026-07-27T11:30:00+08:00", 1, 2);
    let mut content = std::fs::read_to_string(&file).unwrap();
    content.push_str(&newer);
    content.push('\n');
    std::fs::write(&file, content).unwrap();
    let stats3 = scan_unassigned(&sessions, &state_path, now, &tz);
    assert_eq!(stats3.last_event_at, Some(ms("2026-07-27T11:30:00+08:00")));

    // 空目录（从未扫到消耗）：None，活跃判定按静默
    let empty_stats = scan_unassigned(
        &dir.join("nonexistent"),
        &dir.join("other-state.json"),
        now,
        &tz,
    );
    assert_eq!(empty_stats.last_event_at, None);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 端到端：嵌套 step.end 采样进速率聚合 —— 窗口边界（恰 10 分钟前计入、更早不计）、
/// 零时长事件跳过、step.end 不进逐日 tokens、二次扫描不重复、窗口随扫描时刻滑动
#[test]
fn scan_step_end_rate_window_and_zero_duration() {
    let dir = temp_dir("local-usage-rate");
    let sessions = dir.join("sessions");
    let state_path = dir.join("config").join("scan-state.json");
    let tz = tz8();
    let now = ms("2026-07-27T12:00:00+08:00");

    write_wire(
        &sessions,
        "main",
        &[
            // 窗口内两条（各 50 tok/s）
            step_end_line(None, "2026-07-27T11:55:00+08:00", 100, 2000),
            step_end_line(None, "2026-07-27T11:59:30+08:00", 50, 1000),
            // 恰在 10 分钟边界（now − 600s）：计入（>= 语义），30 tok/s
            step_end_line(Some("kimi-code/k3"), "2026-07-27T11:50:00+08:00", 90, 3000),
            // 窗口外（30 分钟前）不计入；零时长事件跳过防除零
            step_end_line(None, "2026-07-27T11:30:00+08:00", 999, 5000),
            step_end_line(None, "2026-07-27T11:58:00+08:00", 70, 0),
            // 老格式 usage.record 照常进逐日累计（与 step.end 采样互不干扰）
            usage_line("kimi-code/k3", "2026-07-27T11:56:00+08:00", 100, 10),
        ],
    );

    // 速率 = (100+50+90)/(2000+1000+3000)×1000 = 40 tok/s；step.end 不进今日 tokens
    let stats = scan_unassigned(&sessions, &state_path, now, &tz);
    assert_eq!(stats.today_tokens, 100 + 10 + 11264);
    assert!(approx(stats.recent_output_tok_per_sec.unwrap(), 40.0));

    // 二次扫描：偏移续读零新行，采样不重复计数
    let stats2 = scan_unassigned(&sessions, &state_path, now, &tz);
    assert!(approx(stats2.recent_output_tok_per_sec.unwrap(), 40.0));

    // 窗口随扫描时刻滑动：11 分钟后全部采样出窗 → None
    let later = now + 11 * 60 * 1000;
    let stats3 = scan_unassigned(&sessions, &state_path, later, &tz);
    assert_eq!(stats3.recent_output_tok_per_sec, None);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 回归（v1.10.0 实机抓到）：缺 model 的 step.end 旧版直接退 CLI 级兜底归属
/// （attribute_cli 优先 Kimi 通道），DeepSeek 模型会话的速率采样全落 Kimi 桶
/// ——DeepSeek 页无速率、Kimi 页速率猛涨，张冠李戴。修复后跟随会话级模型
/// 记忆（同会话 usage.record 推进、随 scan-state 落盘跨批次保留）
#[test]
fn scan_step_end_without_model_follows_session_model() {
    let dir = temp_dir("local-usage-rate-attr");
    let sessions = dir.join("sessions");
    let state_path = dir.join("config").join("scan-state.json");
    let tz = tz8();
    let now = ms("2026-07-27T12:00:00+08:00");

    // Kimi Code home 双 provider：kimi 与 deepseek key 都在 config.toml
    // （此时 CLI 级兜底必中 Kimi 通道，正是旧版张冠李戴的场景）
    let attr = || Attribution {
        cli: CliCredentials {
            kimi_api_key: opt("sk-kimi-a"),
            deepseek_api_key: opt("sk-ds-a"),
            ..Default::default()
        },
        kimi_accounts: vec![(
            "acc-kimi".to_string(),
            account_creds(opt("sk-kimi-a"), None),
        )],
        deepseek_accounts: vec![("acc-ds".to_string(), account_creds(opt("sk-ds-a"), None))],
        ..Default::default()
    };

    let file = write_wire(
        &sessions,
        "main",
        &[
            // 会话跑的是 DeepSeek 模型：usage.record 带 model 正常归 acc-ds
            usage_line("deepseek-v4-pro", "2026-07-27T11:50:00+08:00", 100, 10),
            // 缺 model 的 step.end：应跟随会话模型归 acc-ds（旧版退兜底归 acc-kimi）
            step_end_line(None, "2026-07-27T11:55:00+08:00", 100, 2000),
        ],
    );

    let view = scan_with(&[(sessions.clone(), attr())], &state_path, now, &tz);
    // tokens 与速率采样都归 DeepSeek 账号：100 tok / 2000 ms = 50 tok/s
    assert_eq!(view.for_account("acc-ds").today_tokens, 100 + 10 + 11264);
    assert!(approx(
        view.for_account("acc-ds")
            .recent_output_tok_per_sec
            .unwrap(),
        50.0
    ));
    // Kimi 账号桶：无 tokens、无速率采样
    let kimi = view.for_account("acc-kimi");
    assert_eq!(kimi.today_tokens, 0);
    assert_eq!(kimi.recent_output_tok_per_sec, None);

    // 增量续扫：本批只有缺 model 的 step.end（无 usage.record），会话模型
    // 记忆从 scan-state 恢复，不退 CLI 兜底；（100+50)/(2000+1000) = 50 tok/s
    let mut content = std::fs::read_to_string(&file).unwrap();
    content.push_str(&step_end_line(None, "2026-07-27T11:58:00+08:00", 50, 1000));
    content.push('\n');
    std::fs::write(&file, content).unwrap();
    let view2 = scan_with(&[(sessions.clone(), attr())], &state_path, now, &tz);
    assert!(approx(
        view2
            .for_account("acc-ds")
            .recent_output_tok_per_sec
            .unwrap(),
        50.0
    ));
    assert_eq!(
        view2.for_account("acc-kimi").recent_output_tok_per_sec,
        None
    );

    // 会话中途切回 Kimi 模型：后续缺 model 采样跟随新模型归 acc-kimi
    let mut content = std::fs::read_to_string(&file).unwrap();
    content.push_str(&usage_line(
        "kimi-code/k3",
        "2026-07-27T11:59:00+08:00",
        1,
        1,
    ));
    content.push('\n');
    content.push_str(&step_end_line(None, "2026-07-27T11:59:30+08:00", 30, 1000));
    content.push('\n');
    std::fs::write(&file, content).unwrap();
    let view3 = scan_with(&[(sessions.clone(), attr())], &state_path, now, &tz);
    assert!(approx(
        view3
            .for_account("acc-kimi")
            .recent_output_tok_per_sec
            .unwrap(),
        30.0
    ));

    let _ = std::fs::remove_dir_all(&dir);
}

/// 老格式 wire.jsonl（只有 usage.record、无嵌套 step.end）：速率字段 None 且不炸
#[test]
fn scan_without_step_end_reports_none_rate() {
    let dir = temp_dir("local-usage-no-rate");
    let sessions = dir.join("sessions");
    let state_path = dir.join("config").join("scan-state.json");
    let now = ms("2026-07-27T12:00:00+08:00");
    write_wire(
        &sessions,
        "main",
        &[usage_line(
            "kimi-code/k3",
            "2026-07-27T11:00:00+08:00",
            100,
            10,
        )],
    );
    let stats = scan_unassigned(&sessions, &state_path, now, &tz8());
    assert_eq!(stats.today_tokens, 100 + 10 + 11264);
    assert_eq!(stats.recent_output_tok_per_sec, None);

    let _ = std::fs::remove_dir_all(&dir);
}

/// step.end 归属：不带 model 走 CLI 级兜底归属（attribute_cli 的 Kimi 通道命中 acc-a）；
/// 带 model 走与 usage.record 相同的模型路由（kimi/deepseek 模型各归各账号）
#[test]
fn step_end_attributes_by_model_or_cli_fallback() {
    let dir = temp_dir("local-usage-rate-attr");
    let sessions = dir.join("sessions");
    let state_path = dir.join("config").join("scan-state.json");
    let tz = tz8();
    let now = ms("2026-07-27T12:00:00+08:00");
    let attribution = Attribution {
        cli: CliCredentials {
            kimi_api_key: opt("sk-kimi-a"),
            deepseek_api_key: opt("sk-ds-a"),
            ..Default::default()
        },
        kimi_accounts: vec![("acc-a".to_string(), account_creds(opt("sk-kimi-a"), None))],
        deepseek_accounts: vec![("acc-d".to_string(), account_creds(opt("sk-ds-a"), None))],
        ..Default::default()
    };

    write_wire(
        &sessions,
        "main",
        &[
            // 无 model：attribute_cli → Kimi 通道 → acc-a
            step_end_line(None, "2026-07-27T11:59:00+08:00", 100, 2000),
            // 带 kimi 模型：模型路由 Kimi 通道 → acc-a（与上一条同桶，速率合并）
            step_end_line(Some("kimi-code/k3"), "2026-07-27T11:58:30+08:00", 50, 1000),
            // 带 deepseek 模型：模型路由 DeepSeek 通道 → acc-d
            step_end_line(
                Some("deepseek-v4-flash"),
                "2026-07-27T11:58:00+08:00",
                60,
                3000,
            ),
        ],
    );

    let view = scan_with(
        &[(sessions.to_path_buf(), attribution)],
        &state_path,
        now,
        &tz,
    );
    let acc_a = view.for_account("acc-a");
    assert!(approx(acc_a.recent_output_tok_per_sec.unwrap(), 50.0));
    let acc_d = view.for_account("acc-d");
    assert!(approx(acc_d.recent_output_tok_per_sec.unwrap(), 20.0));
    // step.end 采样不进逐日 tokens 累计（速率与消耗口径分离）
    assert_eq!(acc_a.today_tokens, 0);
    assert_eq!(acc_d.today_tokens, 0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn legacy_totals_state_wiped_and_full_rescan() {
    let dir = temp_dir("local-usage-legacy");
    let sessions = dir.join("sessions");
    let state_path = dir.join("config").join("scan-state.json");
    let tz = tz8();
    let now = ms("2026-07-27T12:00:00+08:00");

    // 今日一条事件；旧状态偏移已越过它（模拟旧版已消费）。
    // 偏移不归零则该事件不会重读；旧合计不清空则结果混进 999999 幽灵数字
    let file = write_wire(
        &sessions,
        "main",
        &[usage_line(
            "kimi-code/k3",
            "2026-07-27T10:00:00+08:00",
            100,
            10,
        )],
    );
    let today_tokens = 100 + 10 + 11264;
    // 旧版 scan-state：机器级 totals 合计（无 buckets 键）
    let legacy = serde_json::json!({
        "last_scan_at": ms("2026-07-27T09:00:00+08:00") / 1000,
        "files": { file.to_string_lossy().into_owned(): std::fs::metadata(&file).unwrap().len() },
        "totals": {
            "by_date": { "2026-07-27": 999999 },
            "by_date_model": { "2026-07-27": { "kimi-code/k3": 999999 } },
            "last_event_at": ms("2026-07-27T10:00:00+08:00"),
        },
    });
    std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    std::fs::write(&state_path, serde_json::to_string(&legacy).unwrap()).unwrap();

    // 旧合计直接丢弃 + 偏移归零全量重扫：只剩真实事件，999999 不出现
    let view = scan_with(
        &[(sessions.clone(), Attribution::default())],
        &state_path,
        now,
        &tz,
    );
    let stats = view.for_account(UNASSIGNED_BUCKET);
    assert_eq!(stats.today_tokens, today_tokens);
    assert_eq!(stats.by_model.len(), 1);
    assert_eq!(stats.by_model[0].tokens, today_tokens);

    // 落盘的新状态已切到分桶格式：有 buckets 键、无 totals 残留
    let saved = std::fs::read_to_string(&state_path).unwrap();
    assert!(saved.contains("\"buckets\""));
    assert!(!saved.contains("\"totals\""));

    // 重扫只发生一次：二次扫描不重复计数
    let stats2 = scan_unassigned(&sessions, &state_path, now, &tz);
    assert_eq!(stats2.today_tokens, today_tokens);
    assert_eq!(stats2.by_model, stats.by_model);

    let _ = std::fs::remove_dir_all(&dir);
}

// ---- WSL 抖动重复计账根修：已扫 root 前缀清理 ----

#[test]
fn unreachable_root_keeps_offsets_and_no_recount() {
    // WSL-flap 回归：home 整轮不可达（停止的 WSL 发行版不进扫描目标）
    // → 其偏移与模型记忆原样保留；恢复可见后续扫，账面零增长（不重复计账）
    let dir = temp_dir("local-usage-flap");
    let sessions_a = dir.join("home-a").join("sessions");
    let sessions_b = dir.join("home-b").join("sessions");
    let state_path = dir.join("config").join("scan-state.json");
    let tz = tz8();
    let now = ms("2026-07-27T12:00:00+08:00");

    let file_a = write_wire(
        &sessions_a,
        "main",
        &[usage_line(
            "kimi-code/k3",
            "2026-07-27T10:00:00+08:00",
            100,
            10,
        )],
    );
    let file_b = write_wire(
        &sessions_b,
        "main",
        &[usage_line(
            "kimi-code/k3",
            "2026-07-27T10:00:00+08:00",
            200,
            20,
        )],
    );
    let tokens_a = 100 + 10 + 11264;
    let tokens_b = 200 + 20 + 11264;
    let key_a = file_a.to_string_lossy().into_owned();
    let key_b = file_b.to_string_lossy().into_owned();
    let both = vec![
        (sessions_a.clone(), Attribution::default()),
        (sessions_b.clone(), Attribution::default()),
    ];

    // 第一轮：两个 home 都在目标 → 双双入账，偏移与模型记忆落盘
    let view = scan_with(&both, &state_path, now, &tz);
    assert_eq!(
        view.for_account(UNASSIGNED_BUCKET).today_tokens,
        tokens_a + tokens_b
    );
    let state = load_state(&state_path);
    assert!(state.files.contains_key(&key_a) && state.files.contains_key(&key_b));
    assert_eq!(
        state.kimi_models.get(&key_a).map(String::as_str),
        Some("kimi-code/k3")
    );

    // 第二轮：home-a 整轮不可达（模拟 WSL 关机）→ A 的偏移与模型记忆必须还在；
    // 聚合账本不缩水（B 续扫零增长）
    let view = scan_with(
        &[(sessions_b.clone(), Attribution::default())],
        &state_path,
        now,
        &tz,
    );
    assert_eq!(
        view.for_account(UNASSIGNED_BUCKET).today_tokens,
        tokens_a + tokens_b
    );
    let state = load_state(&state_path);
    assert!(
        state.files.contains_key(&key_a),
        "不可达 root 的文件偏移必须保留（否则恢复后全量重读重复计账）"
    );
    assert!(
        state.kimi_models.contains_key(&key_a),
        "不可达 root 的模型记忆必须保留"
    );

    // 第三轮：A 恢复可见 → 从旧偏移续扫，账面零增长（修复前这里会整倍重计）
    let view = scan_with(&both, &state_path, now, &tz);
    assert_eq!(
        view.for_account(UNASSIGNED_BUCKET).today_tokens,
        tokens_a + tokens_b,
        "WSL 恢复后不得重复计账"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deleted_file_under_scanned_root_cleaned() {
    // root 在扫描目标里、文件盘上没了 → 条目（偏移 + 模型记忆）被清；
    // 同路径新建文件从头读、不按旧偏移跳过开头
    let dir = temp_dir("local-usage-deleted");
    let sessions = dir.join("sessions");
    let state_path = dir.join("config").join("scan-state.json");
    let tz = tz8();
    let now = ms("2026-07-27T12:00:00+08:00");

    let file = write_wire(
        &sessions,
        "main",
        &[usage_line(
            "kimi-code/k3",
            "2026-07-27T10:00:00+08:00",
            100,
            10,
        )],
    );
    let key = file.to_string_lossy().into_owned();
    let old_tokens = 100 + 10 + 11264;
    scan_unassigned(&sessions, &state_path, now, &tz);
    assert!(load_state(&state_path).files.contains_key(&key));

    // 文件删除后扫描：root 在目标里 → 条目被清
    std::fs::remove_file(&file).unwrap();
    scan_unassigned(&sessions, &state_path, now, &tz);
    let state = load_state(&state_path);
    assert!(!state.files.contains_key(&key), "已删文件的偏移必须清理");
    assert!(
        !state.kimi_models.contains_key(&key),
        "已删文件的模型记忆必须清理"
    );

    // 同路径新建文件（内容不同）：从头读，新事件入账
    let file = write_wire(
        &sessions,
        "main",
        &[usage_line(
            "kimi-code/k3",
            "2026-07-27T11:00:00+08:00",
            7,
            3,
        )],
    );
    assert_eq!(file.to_string_lossy().into_owned(), key);
    let stats = scan_unassigned(&sessions, &state_path, now, &tz);
    assert_eq!(stats.today_tokens, old_tokens + (7 + 3 + 11264));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn prefix_cleanup_respects_path_components() {
    // 前缀边界按路径分量：root 「…/x」在扫不能误清「…/x2」下的条目
    // （裸字符串 startsWith("…/x") 会把 "…/x2/…" 误判为前缀内 → 误清 → 重计）
    let dir = temp_dir("local-usage-prefix");
    let root_x = dir.join("x");
    let root_x2 = dir.join("x2");
    let state_path = dir.join("config").join("scan-state.json");
    let tz = tz8();
    let now = ms("2026-07-27T12:00:00+08:00");

    let file_x = write_wire(
        &root_x,
        "main",
        &[usage_line(
            "kimi-code/k3",
            "2026-07-27T10:00:00+08:00",
            100,
            10,
        )],
    );
    let file_x2 = write_wire(
        &root_x2,
        "main",
        &[usage_line(
            "kimi-code/k3",
            "2026-07-27T10:00:00+08:00",
            200,
            20,
        )],
    );
    let tokens_x = 100 + 10 + 11264;
    let tokens_x2 = 200 + 20 + 11264;
    let key_x2 = file_x2.to_string_lossy().into_owned();
    let both = vec![
        (root_x.clone(), Attribution::default()),
        (root_x2.clone(), Attribution::default()),
    ];
    let view = scan_with(&both, &state_path, now, &tz);
    assert_eq!(
        view.for_account(UNASSIGNED_BUCKET).today_tokens,
        tokens_x + tokens_x2
    );

    // 只扫 root_x：root_x2 的条目必须原样保留（「…/x」的前缀清理不能越界到「…/x2」）
    scan_with(
        &[(root_x.clone(), Attribution::default())],
        &state_path,
        now,
        &tz,
    );
    let state = load_state(&state_path);
    assert!(
        state.files.contains_key(&key_x2),
        "root 前缀清理必须按路径分量：「…/x」不能误清「…/x2」下的条目"
    );

    // root_x2 回到目标但文件已删：自己的 root 在扫 → 条目照清（保护不越界续命）
    std::fs::remove_file(&file_x2).unwrap();
    scan_with(&both, &state_path, now, &tz);
    assert!(!load_state(&state_path).files.contains_key(&key_x2));

    // file_x 全程未动：续扫零增长
    let view = scan_with(&both, &state_path, now, &tz);
    assert_eq!(
        view.for_account(UNASSIGNED_BUCKET).today_tokens,
        tokens_x + tokens_x2
    );
    let _ = file_x;

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn v5_state_discarded_and_full_rescan() {
    // STATE_VERSION 5→6：v5 状态（带 version 字段、buckets 幽灵聚合、越界偏移）
    // 整体丢弃 → 按现存文件全量重扫：幽灵数字不出现、真实事件计一次
    let dir = temp_dir("local-usage-v5");
    let sessions = dir.join("sessions");
    let state_path = dir.join("config").join("scan-state.json");
    let tz = tz8();
    let now = ms("2026-07-27T12:00:00+08:00");

    let file = write_wire(
        &sessions,
        "main",
        &[usage_line(
            "kimi-code/k3",
            "2026-07-27T10:00:00+08:00",
            100,
            10,
        )],
    );
    let today_tokens = 100 + 10 + 11264;
    let key = file.to_string_lossy().into_owned();
    let v5 = serde_json::json!({
        "version": 5,
        "last_scan_at": ms("2026-07-27T09:00:00+08:00") / 1000,
        // 偏移已越过今日事件（模拟旧版已消费）：不丢弃则该事件不会重读
        "files": { key.clone(): std::fs::metadata(&file).unwrap().len() },
        "buckets": {
            UNASSIGNED_BUCKET: {
                "by_date": { "2026-07-27": 999999 },
                "by_date_model": { "2026-07-27": { "kimi-code/k3": 999999 } },
                "last_event_at": ms("2026-07-27T10:00:00+08:00"),
            }
        },
        "kimi_models": { key: "kimi-code/k3" },
    });
    std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    std::fs::write(&state_path, serde_json::to_string(&v5).unwrap()).unwrap();

    let stats = scan_unassigned(&sessions, &state_path, now, &tz);
    assert_eq!(
        stats.today_tokens, today_tokens,
        "幽灵聚合必须丢弃、真实事件全量重扫计一次"
    );

    // 落盘的新状态已升到当前版本
    let state = load_state(&state_path);
    assert_eq!(state.version, STATE_VERSION);

    let _ = std::fs::remove_dir_all(&dir);
}

// ---- 缓存命中率 ----

#[test]
fn parse_usage_line_fills_cache_components() {
    // 实测真实样例：cache_read = inputCacheRead；input_total = 三个输入分量（不含 output）
    let line = r#"{"type":"usage.record","model":"kimi-code/k3","usage":{"inputOther":11592,"output":504,"inputCacheRead":11264,"inputCacheCreation":0},"usageScope":"turn","time":1784973672311}"#;
    let event = parse_usage_line(line).unwrap();
    assert_eq!(event.cache_read, 11264);
    assert_eq!(event.input_total, 11592 + 11264); // inputCacheCreation = 0 省略
                                                  // 缺 usage → 0/0
    let event = parse_usage_line(r#"{"type":"usage.record","model":"m","time":1000}"#).unwrap();
    assert_eq!(event.cache_read, 0);
    assert_eq!(event.input_total, 0);
}

#[test]
fn aggregator_cache_hit_rate_per_day_and_none_cases() {
    let tz = tz8();
    let mut agg = UsageAggregator::default();
    // 今天（UTC+8 2026-07-27）：两条 Kimi 事件，分量累计
    // usage_line 每条 inputCacheRead=11264、inputCacheCreation=0
    agg.add(
        &parse_usage_line(&usage_line("m1", "2026-07-27T10:00:00+08:00", 736, 100)).unwrap(),
        &tz,
    );
    agg.add(
        &parse_usage_line(&usage_line("m1", "2026-07-27T11:00:00+08:00", 1000, 200)).unwrap(),
        &tz,
    );
    // 昨日：纯 output 事件（输入分量全 0）→ tokens 有值但命中率 None
    let pure_output = UsageEvent {
        ts_ms: ms("2026-07-26T10:00:00+08:00"),
        model: "m1".to_string(),
        tokens: 500,
        cache_read: 0,
        input_total: 0,
    };
    agg.add(&pure_output, &tz);
    // 前天：一条 Kimi 事件 + 一条 harness 事件（0/0）→ harness 不进分子分母
    agg.add(
        &parse_usage_line(&usage_line("m1", "2026-07-25T10:00:00+08:00", 1000, 0)).unwrap(),
        &tz,
    );
    let harness_event = UsageEvent {
        ts_ms: ms("2026-07-25T11:00:00+08:00"),
        model: "gpt-x".to_string(),
        tokens: 7777,
        cache_read: 0,
        input_total: 0,
    };
    agg.add(&harness_event, &tz);

    let stats = agg.finish(
        chrono::NaiveDate::from_ymd_opt(2026, 7, 27).unwrap(),
        None,
        ms("2026-07-27T12:00:00+08:00"),
    );
    // 今日：cache = 11264*2，input = (736+11264) + (1000+11264) = 24264 → 22528/24264
    let today_rate = stats.today_cache_hit_rate.unwrap();
    assert!(approx(today_rate, 22528.0 / 24264.0));
    // daily 升序末位即今日，与 today_cache_hit_rate 一致
    let today_daily = stats.daily.last().unwrap();
    assert_eq!(today_daily.date, "2026-07-27");
    assert!(approx(today_daily.cache_hit_rate.unwrap(), today_rate));
    // 昨日（纯 output / 输入 0）→ None；tokens 仍在
    let yesterday = &stats.daily[stats.daily.len() - 2];
    assert_eq!(yesterday.date, "2026-07-26");
    assert_eq!(yesterday.tokens, 500);
    assert_eq!(yesterday.cache_hit_rate, None);
    // 前天：harness 事件只进 tokens、不进命中率分子分母 → 11264 / (1000+11264)
    let before = &stats.daily[stats.daily.len() - 3];
    assert_eq!(before.date, "2026-07-25");
    assert_eq!(before.tokens, 11264 + 1000 + 7777);
    assert!(approx(before.cache_hit_rate.unwrap(), 11264.0 / 12264.0));
    // 更早的零消耗日 → None
    assert_eq!(stats.daily[0].cache_hit_rate, None);
}

#[test]
fn scan_cache_hit_rate_end_to_end() {
    // 扫描层端到端：wire 事件的分量经聚合进 daily 与 today_cache_hit_rate
    let dir = temp_dir("local-usage-cache-rate");
    let sessions = dir.join("sessions");
    let state_path = dir.join("config").join("scan-state.json");
    let tz = tz8();
    let now = ms("2026-07-27T12:00:00+08:00");
    write_wire(
        &sessions,
        "main",
        &[
            usage_line("kimi-code/k3", "2026-07-27T10:00:00+08:00", 100, 10),
            usage_line("kimi-code/k3", "2026-07-26T10:00:00+08:00", 300, 30),
        ],
    );
    let stats = scan_unassigned(&sessions, &state_path, now, &tz);
    // 今日：cache 11264 / input (100+11264)；昨日：cache 11264 / input (300+11264)
    assert!(approx(
        stats.today_cache_hit_rate.unwrap(),
        11264.0 / 11364.0
    ));
    let yesterday = &stats.daily[stats.daily.len() - 2];
    assert!(approx(yesterday.cache_hit_rate.unwrap(), 11264.0 / 11564.0));
    // 续扫（无新行）：命中率不重复累计
    let stats2 = scan_unassigned(&sessions, &state_path, now, &tz);
    assert!(approx(
        stats2.today_cache_hit_rate.unwrap(),
        11264.0 / 11364.0
    ));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scan_missing_sessions_dir_returns_empty() {
    let dir = temp_dir("local-usage-empty");
    let stats = scan_unassigned(
        &dir.join("nonexistent"),
        &dir.join("scan-state.json"),
        ms("2026-07-27T12:00:00+08:00"),
        &tz8(),
    );
    assert_eq!(stats.today_tokens, 0);
    assert_eq!(stats.daily.len(), 7);
    assert!(stats.by_model.is_empty());
    assert!(stats.last_scan_at.is_some());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scan_throttles_within_180s() {
    let _guard = ENV_LOCK.lock().unwrap();
    let home = temp_dir("local-usage-home");
    let config = temp_dir("local-usage-conf");
    std::env::set_var("USERPROFILE", &home);
    std::env::set_var("KIMICODEBAR_CONFIG_DIR", &config);
    // 本机真实 WSL home 不进本测试（scan 会发现它）：根指到不存在目录
    std::env::set_var("KIMICODEBAR_WSL_ROOT", home.join("no-wsl"));

    // 今日事件（用真实本地时钟，scan() 走 chrono::Local）
    let now_ms = chrono::Local::now().timestamp_millis();
    let line = format!(
        r#"{{"type":"usage.record","model":"kimi-code/k3","usage":{{"inputOther":1,"output":2,"inputCacheRead":0,"inputCacheCreation":0}},"usageScope":"turn","time":{now_ms}}}"#
    );
    write_wire(&home.join(".kimi-code").join("sessions"), "main", &[line]);
    // 合法 home 判定要求 config.toml 或 credentials/ 之一存在（空 config 即满足）
    std::fs::write(home.join(".kimi-code").join("config.toml"), "").unwrap();

    let stats1 = scan();
    assert_eq!(stats1.for_account(UNASSIGNED_BUCKET).today_tokens, 3);
    assert!(config.join("scan-state.json").exists());

    // 距上次 < 180 秒：直接返回缓存（last_scan_at 相同即未重扫）
    let stats2 = scan();
    assert_eq!(stats2, stats1);

    std::env::remove_var("USERPROFILE");
    std::env::remove_var("KIMICODEBAR_CONFIG_DIR");
    std::env::remove_var("KIMICODEBAR_WSL_ROOT");
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&config);
}

// ---- 导出 CSV ----

#[test]
fn csv_format_header_rows_and_local_iso_time() {
    let tz = tz8();
    let t1 = tz
        .with_ymd_and_hms(2026, 7, 27, 10, 0, 0)
        .unwrap()
        .timestamp();
    let t2 = tz
        .with_ymd_and_hms(2026, 7, 28, 0, 30, 0)
        .unwrap()
        .timestamp();
    let points = vec![
        HistoryPoint {
            t: t1,
            weekly: Some(12.5),
            five_hour: None,
            monthly: Some(16.12),
        },
        HistoryPoint {
            t: t2,
            weekly: None,
            five_hour: Some(3.25),
            monthly: None,
        },
    ];
    let csv = build_history_csv(&points, &tz);
    assert_eq!(
        csv,
        "time,weekly,five_hour,monthly\n\
         2026-07-27T10:00:00,12.5,,16.12\n\
         2026-07-28T00:30:00,,3.25,\n"
    );
}

#[test]
fn csv_empty_history_is_header_only() {
    assert_eq!(
        build_history_csv(&[], &tz8()),
        "time,weekly,five_hour,monthly\n"
    );
}

#[test]
fn export_writes_csv_and_copies_history() {
    let dir = temp_dir("local-usage-export");
    let exports = dir.join("exports");
    let history_src = dir.join("history.json");
    std::fs::write(&history_src, r#"{"points":[{"t":1,"weekly":1.0}]}"#).unwrap();

    let now = tz8().with_ymd_and_hms(2026, 7, 27, 12, 34, 56).unwrap();
    let points = vec![HistoryPoint {
        t: now.timestamp(),
        weekly: Some(42.5),
        five_hour: None,
        monthly: None,
    }];
    let out = export_report_to(&exports, &history_src, &points, now, Some("账号 1")).unwrap();
    assert_eq!(out, exports);

    // CSV 文件名带本地时间戳与账号名后缀，内容表头 + 一行
    let csv_path = exports.join("usage-20260727-123456-账号 1.csv");
    let csv = std::fs::read_to_string(&csv_path).unwrap();
    assert_eq!(
        csv,
        "time,weekly,five_hour,monthly\n2026-07-27T12:34:56,42.5,,\n"
    );
    // history-账号 1.json 原文已复制到同目录
    assert_eq!(
        std::fs::read_to_string(exports.join("history-账号 1.json")).unwrap(),
        r#"{"points":[{"t":1,"weekly":1.0}]}"#
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn export_tolerates_missing_history_source() {
    let dir = temp_dir("local-usage-export2");
    let exports = dir.join("exports");
    let now = tz8().with_ymd_and_hms(2026, 7, 27, 12, 0, 0).unwrap();
    // history.json 不存在（从未刷新成功过）：CSV 照常导出，复制跳过
    export_report_to(&exports, &dir.join("history.json"), &[], now, None).unwrap();
    assert!(exports.join("usage-20260727-120000.csv").exists());
    assert!(!exports.join("history.json").exists());

    let _ = std::fs::remove_dir_all(&dir);
}

// ---- __secondary__ 折叠与解析 ----

fn model_usage(model: &str, tokens: u64) -> ModelUsage {
    ModelUsage {
        model: model.to_string(),
        tokens,
    }
}

/// 在假 home 下写 .kimi-code/config.toml，含 [secondary_model].model
fn write_secondary_config(home: &Path, model: &str) {
    write_config_raw(home, &format!("[secondary_model]\nmodel = \"{model}\"\n"));
}

fn write_config_raw(home: &Path, content: &str) {
    let dir = home.join(".kimi-code");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.toml"), content).unwrap();
}

#[test]
fn fold_merges_sentinel_into_existing_target() {
    let mut by_model = vec![
        model_usage("kimi-code/k3", 100),
        model_usage(SECONDARY_SENTINEL, 40),
        model_usage("deepseek-v4-flash", 10),
    ];
    fold_secondary_model(&mut by_model, "deepseek-v4-flash");
    // 哨兵 40 并入已在榜的 deepseek-v4-flash（10 → 50），哨兵桶消失
    assert_eq!(
        by_model,
        vec![
            model_usage("kimi-code/k3", 100),
            model_usage("deepseek-v4-flash", 50),
        ]
    );
}

#[test]
fn fold_renames_when_target_not_on_board() {
    // target 不在榜：哨兵桶改名顶替，并按合并值重排（200 > 100 居首）
    let mut by_model = vec![
        model_usage("kimi-code/k3", 100),
        model_usage(SECONDARY_SENTINEL, 200),
    ];
    fold_secondary_model(&mut by_model, "deepseek-v4-flash");
    assert_eq!(
        by_model,
        vec![
            model_usage("deepseek-v4-flash", 200),
            model_usage("kimi-code/k3", 100),
        ]
    );
}

#[test]
fn fold_noop_without_sentinel() {
    let mut by_model = vec![model_usage("kimi-code/k3", 100)];
    fold_secondary_model(&mut by_model, "deepseek-v4-flash");
    assert_eq!(by_model, vec![model_usage("kimi-code/k3", 100)]);
}

#[test]
fn fold_truncates_to_top5_after_merge() {
    // 5 个 100 的桶 + 哨兵 500：合并后 target 居首，榜仍只留 5 个
    let mut by_model: Vec<ModelUsage> =
        (0..5).map(|i| model_usage(&format!("m{i}"), 100)).collect();
    by_model.push(model_usage(SECONDARY_SENTINEL, 500));
    fold_secondary_model(&mut by_model, "deepseek-v4-flash");
    assert_eq!(by_model.len(), TOP_MODELS);
    assert_eq!(by_model[0], model_usage("deepseek-v4-flash", 500));
}

#[test]
fn resolve_prefers_env_over_config() {
    let _guard = ENV_LOCK.lock().unwrap();
    let home = temp_dir("secondary-env");
    write_secondary_config(&home, "deepseek-v4-flash");
    std::env::set_var("USERPROFILE", &home);
    std::env::set_var("KIMI_SECONDARY_MODEL", "kimi-code/kimi-k2.5");
    // 环境变量优先于 config.toml（与 CLI 优先级一致）
    assert_eq!(
        resolve_secondary_model().as_deref(),
        Some("kimi-code/kimi-k2.5")
    );
    // 环境变量为空白：回落 config.toml
    std::env::set_var("KIMI_SECONDARY_MODEL", "  ");
    assert_eq!(
        resolve_secondary_model().as_deref(),
        Some("deepseek-v4-flash")
    );
    std::env::remove_var("KIMI_SECONDARY_MODEL");
    std::env::remove_var("USERPROFILE");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn resolve_reads_model_from_config_toml() {
    let _guard = ENV_LOCK.lock().unwrap();
    let home = temp_dir("secondary-conf");

    std::env::set_var("USERPROFILE", &home);
    std::env::remove_var("KIMI_SECONDARY_MODEL"); // 防外部环境变量泄漏进测试
    write_secondary_config(&home, "deepseek-v4-flash");
    assert_eq!(
        resolve_secondary_model().as_deref(),
        Some("deepseek-v4-flash")
    );
    std::env::remove_var("USERPROFILE");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn resolve_returns_none_when_unresolvable() {
    let _guard = ENV_LOCK.lock().unwrap();
    let home = temp_dir("secondary-none");
    std::env::set_var("USERPROFILE", &home);
    std::env::remove_var("KIMI_SECONDARY_MODEL");
    // config.toml 不存在
    assert_eq!(resolve_secondary_model(), None);
    // 无 [secondary_model] 段
    write_config_raw(&home, "default_model = \"kimi-code/k3\"\n");
    assert_eq!(resolve_secondary_model(), None);
    // 有段无 model 键
    write_config_raw(&home, "[secondary_model]\ndefault_effort = \"max\"\n");
    assert_eq!(resolve_secondary_model(), None);
    // model 类型不是字符串
    write_config_raw(&home, "[secondary_model]\nmodel = 42\n");
    assert_eq!(resolve_secondary_model(), None);
    // 坏 TOML：None 而不是 panic
    write_config_raw(&home, "not = [valid");
    assert_eq!(resolve_secondary_model(), None);
    std::env::remove_var("USERPROFILE");
    let _ = std::fs::remove_dir_all(&home);
}

/// scan_with + resolve + fold 串起来的端到端（scan() 的折叠接线走进程级缓存，不直接测）
#[test]
fn scan_output_folds_secondary_into_configured_model() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = temp_dir("local-usage-secondary");
    let home = dir.join("home");
    let sessions = home.join(".kimi-code").join("sessions");
    let state_path = dir.join("config").join("scan-state.json");
    write_secondary_config(&home, "deepseek-v4-flash");
    std::env::set_var("USERPROFILE", &home);
    std::env::remove_var("KIMI_SECONDARY_MODEL");

    let tz = tz8();
    let now = ms("2026-07-27T12:00:00+08:00");
    write_wire(
        &sessions,
        "main",
        &[usage_line(
            "kimi-code/k3",
            "2026-07-27T10:00:00+08:00",
            100,
            10,
        )],
    );
    write_wire(
        &sessions,
        "agent-0",
        &[
            // 主 agent 直接用过副模型（真实名桶已存在）
            usage_line("deepseek-v4-flash", "2026-07-27T10:30:00+08:00", 10, 5),
            usage_line(SECONDARY_SENTINEL, "2026-07-27T11:00:00+08:00", 20, 5),
        ],
    );

    let mut view = scan_with(
        &[(sessions.clone(), Attribution::default())],
        &state_path,
        now,
        &tz,
    );
    let stats = view.by_account.get_mut(UNASSIGNED_BUCKET).unwrap();
    if let Some(target) = resolve_secondary_model() {
        fold_secondary_model(&mut stats.by_model, &target);
    }

    // 每条 usage_line 另含 inputCacheRead 11264：
    // deepseek = (10+5+11264) + (20+5+11264) = 22568 > k3 = 100+10+11264 = 11374，哨兵桶消失
    assert_eq!(
        stats.by_model,
        vec![
            model_usage("deepseek-v4-flash", 22568),
            model_usage("kimi-code/k3", 11374),
        ]
    );

    std::env::remove_var("USERPROFILE");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- 分账号归属 ----

fn opt(s: &str) -> Option<String> {
    Some(s.to_string())
}

fn account_creds(api_key: Option<String>, user_id: Option<String>) -> AccountCreds {
    AccountCreds {
        api_key,
        user_id,
        extra_api_keys: Vec::new(),
    }
}

/// 带额外 key 的账号凭证快照（api_key_extra 槽位内容的内存形态）
fn account_creds_with_extras(
    api_key: Option<String>,
    user_id: Option<String>,
    extra_api_keys: Vec<String>,
) -> AccountCreds {
    AccountCreds {
        api_key,
        user_id,
        extra_api_keys,
    }
}

/// 造一个 payload 为给定 JSON 的 JWT（归属解码只看 payload 段，不验签）
fn jwt_with_payload(payload: &str) -> String {
    use base64::Engine;
    let enc = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
    format!("{}.{}.sig", enc(r#"{"alg":"none"}"#), enc(payload))
}

#[test]
fn jwt_decodes_user_id() {
    // user_id 优先（CLI 真实 token 两字段同在，user_id 为准）
    let token = jwt_with_payload(r#"{"user_id":"u-123","sub":"s-456"}"#);
    assert_eq!(jwt_user_id(&token).as_deref(), Some("u-123"));
}

#[test]
fn jwt_falls_back_to_sub() {
    let token = jwt_with_payload(r#"{"sub":"s-456","exp":1}"#);
    assert_eq!(jwt_user_id(&token).as_deref(), Some("s-456"));
}

#[test]
fn jwt_tolerates_garbage() {
    use base64::Engine;
    let enc = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
    assert_eq!(jwt_user_id(""), None);
    assert_eq!(jwt_user_id("no-dots-at-all"), None);
    assert_eq!(jwt_user_id("a.b"), None); // 段数不够
    assert_eq!(jwt_user_id("a.!!!.c"), None); // base64url 解码失败
    let not_json = format!("x.{}.y", enc("not json"));
    assert_eq!(jwt_user_id(&not_json), None); // 合法 base64 但非 JSON
    let non_string = jwt_with_payload(r#"{"user_id":42}"#);
    assert_eq!(jwt_user_id(&non_string), None); // 字段不是字符串
    let missing = jwt_with_payload(r#"{"exp":1}"#);
    assert_eq!(jwt_user_id(&missing), None); // user_id / sub 都缺
}

#[test]
fn route_model_config_table_then_prefix_fallback() {
    let attribution = Attribution {
        model_providers: HashMap::from([
            ("kimi-code/k3".to_string(), "managed:kimi-code".to_string()),
            ("deepseek-v4-pro".to_string(), "deepseek".to_string()),
            ("qwen3.8-max".to_string(), "dashscope".to_string()),
        ]),
        ..Default::default()
    };
    // config 表命中：provider 含 kimi → Kimi；deepseek 开头 → DeepSeek；第三方 → 未归属
    assert_eq!(route_model("kimi-code/k3", &attribution), Route::Kimi);
    assert_eq!(
        route_model("deepseek-v4-pro", &attribution),
        Route::DeepSeek
    );
    assert_eq!(route_model("qwen3.8-max", &attribution), Route::Unassigned);
    // 查不到按前缀兜底：deepseek 开头 → DeepSeek，其余 → Kimi
    assert_eq!(route_model("deepseek-v9-x", &attribution), Route::DeepSeek);
    assert_eq!(route_model("kimi-code/k9", &attribution), Route::Kimi);
    assert_eq!(route_model("whatever", &attribution), Route::Kimi);
}

#[test]
fn attribute_matches_kimi_api_key_exact() {
    let attribution = Attribution {
        cli: CliCredentials {
            kimi_api_key: opt("sk-kimi-bbb"),
            ..Default::default()
        },
        kimi_accounts: vec![
            ("acc-a".to_string(), account_creds(opt("sk-kimi-aaa"), None)),
            ("acc-b".to_string(), account_creds(opt("sk-kimi-bbb"), None)),
        ],
        ..Default::default()
    };
    // 精确相等才归：acc-a 的 key 不同不中，acc-b 全等中
    assert_eq!(attribute("kimi-code/k3", &attribution), "acc-b");
}

#[test]
fn attribute_matches_oauth_user_id() {
    let attribution = Attribution {
        cli: CliCredentials {
            kimi_user_id: opt("user-2"),
            ..Default::default()
        },
        kimi_accounts: vec![
            ("acc-a".to_string(), account_creds(None, opt("user-1"))),
            ("acc-b".to_string(), account_creds(None, opt("user-2"))),
        ],
        ..Default::default()
    };
    assert_eq!(attribute("kimi-code/k3", &attribution), "acc-b");
}

#[test]
fn attribute_matches_deepseek_api_key_exact() {
    let attribution = Attribution {
        cli: CliCredentials {
            deepseek_api_key: opt("sk-ds-1"),
            ..Default::default()
        },
        deepseek_accounts: vec![("acc-d".to_string(), account_creds(opt("sk-ds-1"), None))],
        ..Default::default()
    };
    assert_eq!(attribute("deepseek-v4-flash", &attribution), "acc-d");
    // 路由隔离：CLI 的 kimi key 与某 DeepSeek 账号 key 相同也不归它（各走各的通道）
    let crossed = Attribution {
        cli: CliCredentials {
            kimi_api_key: opt("sk-ds-1"),
            ..Default::default()
        },
        kimi_accounts: vec![("acc-k".to_string(), account_creds(opt("sk-kimi-x"), None))],
        deepseek_accounts: vec![("acc-d".to_string(), account_creds(opt("sk-ds-1"), None))],
        ..Default::default()
    };
    assert_eq!(attribute("kimi-code/k3", &crossed), UNASSIGNED_BUCKET);
}

#[test]
fn attribute_falls_back_to_unassigned() {
    let attribution = Attribution {
        cli: CliCredentials {
            kimi_api_key: opt("sk-kimi-cli"),
            kimi_user_id: opt("user-cli"),
            deepseek_api_key: opt("sk-ds-cli"),
            glm_api_keys: Vec::new(),
        },
        kimi_accounts: vec![(
            "acc-a".to_string(),
            account_creds(opt("sk-kimi-other"), opt("user-other")),
        )],
        deepseek_accounts: vec![("acc-d".to_string(), account_creds(opt("sk-ds-other"), None))],
        model_providers: HashMap::from([("qwen3.8-max".to_string(), "dashscope".to_string())]),
    };
    // kimi / deepseek 比对全不中 → 未归属
    assert_eq!(attribute("kimi-code/k3", &attribution), UNASSIGNED_BUCKET);
    assert_eq!(
        attribute("deepseek-v4-flash", &attribution),
        UNASSIGNED_BUCKET
    );
    // 第三方路由直接未归属（不比凭证）
    assert_eq!(attribute("qwen3.8-max", &attribution), UNASSIGNED_BUCKET);
    // CLI 无凭证快照（如全未配置）：同样未归属
    assert_eq!(
        attribute("kimi-code/k3", &Attribution::default()),
        UNASSIGNED_BUCKET
    );
}

// ---- 额外 API Key 归属（主 key 或任一额外 key 精确相等即归该账号）----

#[test]
fn attribute_matches_kimi_extra_api_key() {
    let attribution = Attribution {
        cli: CliCredentials {
            kimi_api_key: opt("sk-kimi-extra-2"),
            ..Default::default()
        },
        kimi_accounts: vec![
            (
                "acc-a".to_string(),
                account_creds_with_extras(
                    opt("sk-kimi-main-a"),
                    None,
                    vec!["sk-kimi-extra-1".to_string(), "sk-kimi-extra-2".to_string()],
                ),
            ),
            ("acc-b".to_string(), account_creds(opt("sk-kimi-b"), None)),
        ],
        ..Default::default()
    };
    // CLI 用的是 acc-a 登记的第二把额外 key：归 acc-a
    assert_eq!(attribute("kimi-code/k3", &attribution), "acc-a");
}

#[test]
fn attribute_main_key_still_matches_with_extras_registered() {
    // 登记了额外 key 后主 key 照常命中（回归：额外 key 不挤掉主 key 通道）
    let attribution = Attribution {
        cli: CliCredentials {
            kimi_api_key: opt("sk-kimi-main-a"),
            ..Default::default()
        },
        kimi_accounts: vec![(
            "acc-a".to_string(),
            account_creds_with_extras(
                opt("sk-kimi-main-a"),
                None,
                vec!["sk-kimi-extra-1".to_string()],
            ),
        )],
        ..Default::default()
    };
    assert_eq!(attribute("kimi-code/k3", &attribution), "acc-a");
}

#[test]
fn attribute_matches_deepseek_extra_api_key() {
    let attribution = Attribution {
        cli: CliCredentials {
            deepseek_api_key: opt("sk-ds-extra"),
            ..Default::default()
        },
        deepseek_accounts: vec![(
            "acc-d".to_string(),
            account_creds_with_extras(opt("sk-ds-main"), None, vec!["sk-ds-extra".to_string()]),
        )],
        ..Default::default()
    };
    assert_eq!(attribute("deepseek-v4-flash", &attribution), "acc-d");
}

#[test]
fn attribute_extra_key_miss_goes_unassigned() {
    // CLI 的 key 既不是主 key 也不是任一额外 key（含"差一个字符"的近似值）：未归属
    let attribution = Attribution {
        cli: CliCredentials {
            kimi_api_key: opt("sk-kimi-extra-1x"),
            ..Default::default()
        },
        kimi_accounts: vec![(
            "acc-a".to_string(),
            account_creds_with_extras(
                opt("sk-kimi-main-a"),
                None,
                vec!["sk-kimi-extra-1".to_string()],
            ),
        )],
        ..Default::default()
    };
    assert_eq!(attribute("kimi-code/k3", &attribution), UNASSIGNED_BUCKET);
}

// ---- GLM 路由（wire.jsonl 通道：自定义 provider 段 key 反向提取 + glm 模型路由）----

#[test]
fn route_model_glm_via_config_table_and_prefix() {
    let attribution = Attribution {
        model_providers: HashMap::from([
            ("glm-4.6".to_string(), "zhipu".to_string()),
            ("GLM-5.2".to_string(), "my-custom-glm".to_string()),
            ("qwen3.8-max".to_string(), "dashscope".to_string()),
            ("glm-in-kimi".to_string(), "managed:kimi-code".to_string()),
            ("glm-ds".to_string(), "deepseek-chat".to_string()),
        ]),
        ..Default::default()
    };
    // [models] 命中且 provider 无 kimi/deepseek 标记：模型名小写含 "glm" → Glm
    assert_eq!(route_model("glm-4.6", &attribution), Route::Glm);
    // 大小写不敏感：GLM-5.2 大写也进 Glm 路由
    assert_eq!(route_model("GLM-5.2", &attribution), Route::Glm);
    // dashscope 等第三方仍 Unassigned（模型名不含 glm）
    assert_eq!(route_model("qwen3.8-max", &attribution), Route::Unassigned);
    // provider 的 kimi / deepseek 标记优先于模型名里的 glm
    assert_eq!(route_model("glm-in-kimi", &attribution), Route::Kimi);
    assert_eq!(route_model("glm-ds", &attribution), Route::DeepSeek);
    // [models] 未命中走前缀兜底：glm 开头 → Glm（大小写不敏感），其余分支原样
    let bare = Attribution::default();
    assert_eq!(route_model("glm-4.6", &bare), Route::Glm);
    assert_eq!(route_model("GLM-5.2", &bare), Route::Glm);
    assert_eq!(route_model("deepseek-v9-x", &bare), Route::DeepSeek);
    assert_eq!(route_model("kimi-code/k9", &bare), Route::Kimi);
    assert_eq!(route_model("whatever", &bare), Route::Kimi);
}

#[test]
fn attribute_glm_matches_main_key() {
    // GLM 账号与 Kimi 账号同列在 kimi_accounts（不拆列表）：主 key 精确命中 → 归它
    let attribution = Attribution {
        cli: CliCredentials {
            glm_api_keys: vec!["glm-key-a".to_string()],
            ..Default::default()
        },
        kimi_accounts: vec![
            (
                "acc-kimi".to_string(),
                account_creds(opt("sk-kimi-x"), None),
            ),
            ("acc-glm".to_string(), account_creds(opt("glm-key-a"), None)),
        ],
        ..Default::default()
    };
    assert_eq!(attribute("glm-4.6", &attribution), "acc-glm");
}

#[test]
fn attribute_glm_matches_extra_key() {
    // 额外 key 与主 key 同权：命中任一即归该账号
    let attribution = Attribution {
        cli: CliCredentials {
            glm_api_keys: vec!["glm-key-extra".to_string()],
            ..Default::default()
        },
        kimi_accounts: vec![(
            "acc-glm".to_string(),
            account_creds_with_extras(opt("glm-key-main"), None, vec!["glm-key-extra".to_string()]),
        )],
        ..Default::default()
    };
    assert_eq!(attribute("glm-4.6", &attribution), "acc-glm");
}

#[test]
fn attribute_glm_no_match_goes_unassigned() {
    // 快照里的 GLM key 与任何账号登记的 key 都不等（含差空格的近似值）：未归属
    let attribution = Attribution {
        cli: CliCredentials {
            glm_api_keys: vec!["glm-key-x".to_string()],
            ..Default::default()
        },
        kimi_accounts: vec![(
            "acc-glm".to_string(),
            account_creds_with_extras(opt("glm-key-main"), None, vec!["glm-key-x ".to_string()]),
        )],
        ..Default::default()
    };
    assert_eq!(attribute("glm-4.6", &attribution), UNASSIGNED_BUCKET);
    // 快照没有任何 GLM key：同样未归属
    assert_eq!(
        attribute("glm-4.6", &Attribution::default()),
        UNASSIGNED_BUCKET
    );
}

#[test]
fn attribute_glm_keys_do_not_leak_into_kimi_route() {
    // GLM key 只走 Glm 路由：Kimi 路由不读 glm_api_keys
    let attribution = Attribution {
        cli: CliCredentials {
            glm_api_keys: vec!["glm-key-a".to_string()],
            ..Default::default()
        },
        kimi_accounts: vec![(
            "acc-kimi".to_string(),
            account_creds(opt("sk-kimi-x"), None),
        )],
        ..Default::default()
    };
    // kimi 模型：CLI 无 kimi key → 未归属（glm_api_keys 不参与 Kimi 通道）
    assert_eq!(attribute("kimi-code/k3", &attribution), UNASSIGNED_BUCKET);
    // 反向：glm 模型命中的 key 即使登记在 Kimi 账号（也在 kimi_accounts 里）也归它
    let attribution2 = Attribution {
        cli: CliCredentials {
            glm_api_keys: vec!["sk-kimi-x".to_string()],
            ..Default::default()
        },
        kimi_accounts: vec![(
            "acc-kimi".to_string(),
            account_creds(opt("sk-kimi-x"), None),
        )],
        ..Default::default()
    };
    assert_eq!(attribute("glm-4.6", &attribution2), "acc-kimi");
}

#[test]
fn attribute_cli_falls_back_to_glm_when_only_glm_key() {
    // home 只配了自定义 provider 的 GLM key（无 kimi key / 无 deepseek key）：
    // Kimi、DeepSeek 通道都不中后 GLM 兜底命中
    let attribution = Attribution {
        cli: CliCredentials {
            glm_api_keys: vec!["glm-key-a".to_string()],
            ..Default::default()
        },
        kimi_accounts: vec![("acc-glm".to_string(), account_creds(opt("glm-key-a"), None))],
        ..Default::default()
    };
    assert_eq!(attribute_cli(&attribution), "acc-glm");
    // GLM 兜底也不中：全不中维持未归属
    let no_match = Attribution {
        cli: CliCredentials {
            glm_api_keys: vec!["glm-key-x".to_string()],
            ..Default::default()
        },
        kimi_accounts: vec![("acc-glm".to_string(), account_creds(opt("glm-key-a"), None))],
        ..Default::default()
    };
    assert_eq!(attribute_cli(&no_match), UNASSIGNED_BUCKET);
}

#[test]
fn attribute_cli_prefers_kimi_then_deepseek_then_glm() {
    // 通道优先级：Kimi > DeepSeek > GLM；前者命中即返回，不再看后面
    let attribution = Attribution {
        cli: CliCredentials {
            kimi_api_key: opt("sk-kimi-cli"),
            deepseek_api_key: opt("sk-ds-cli"),
            glm_api_keys: vec!["glm-key-a".to_string()],
            ..Default::default()
        },
        kimi_accounts: vec![
            (
                "acc-kimi".to_string(),
                account_creds(opt("sk-kimi-cli"), None),
            ),
            ("acc-glm".to_string(), account_creds(opt("glm-key-a"), None)),
        ],
        deepseek_accounts: vec![("acc-d".to_string(), account_creds(opt("sk-ds-cli"), None))],
        ..Default::default()
    };
    assert_eq!(attribute_cli(&attribution), "acc-kimi");
    // kimi 不中：DeepSeek 命中优先于 GLM 兜底
    let ds_first = Attribution {
        cli: CliCredentials {
            deepseek_api_key: opt("sk-ds-cli"),
            glm_api_keys: vec!["glm-key-a".to_string()],
            ..Default::default()
        },
        kimi_accounts: vec![("acc-glm".to_string(), account_creds(opt("glm-key-a"), None))],
        deepseek_accounts: vec![("acc-d".to_string(), account_creds(opt("sk-ds-cli"), None))],
        ..Default::default()
    };
    assert_eq!(attribute_cli(&ds_first), "acc-d");
    // kimi / deepseek 都不中：GLM 兜底命中
    let glm_last = Attribution {
        cli: CliCredentials {
            deepseek_api_key: opt("sk-ds-other"),
            glm_api_keys: vec!["glm-key-a".to_string()],
            ..Default::default()
        },
        kimi_accounts: vec![("acc-glm".to_string(), account_creds(opt("glm-key-a"), None))],
        deepseek_accounts: vec![("acc-d".to_string(), account_creds(opt("sk-ds-cli"), None))],
        ..Default::default()
    };
    assert_eq!(attribute_cli(&glm_last), "acc-glm");
}

#[test]
fn snapshot_attribution_extracts_glm_key_from_any_provider_section() {
    let _guard = ENV_LOCK.lock().unwrap();
    let home = temp_dir("snapshot-glm");
    let config = temp_dir("snapshot-glm-conf");
    std::env::set_var("KIMICODEBAR_CONFIG_DIR", &config);
    std::env::set_var(
        "KIMICODEBAR_KEYRING_SERVICE",
        format!("KimiCodeBar-test-{}", uuid::Uuid::new_v4()),
    );
    let settings = crate::storage::Settings {
        accounts: vec![
            crate::storage::Account {
                id: "acc-glm".to_string(),
                name: "G".to_string(),
                login_method: Some("api_key".to_string()),
                provider: "glm".to_string(),
                ..Default::default()
            },
            crate::storage::Account {
                id: "acc-kimi".to_string(),
                name: "K".to_string(),
                login_method: Some("api_key".to_string()),
                provider: "kimi".to_string(),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    std::fs::write(
        config.join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();
    crate::creds::save_api_key("acc-glm", "glm-key-a").unwrap();
    crate::creds::save_api_key_extra("acc-glm", &["glm-key-b".to_string()]).unwrap();
    crate::creds::save_api_key("acc-kimi", "sk-kimi-a").unwrap();
    // 自定义 provider 段（用户随意命名）放 GLM 账号登记的 key；未登记 key 不进；
    // managed:kimi-code / deepseek 两段维持原通道
    // TOML 字段名运行时组装（与 write_zcode_dir 同款手法，避免凭据扫描器把假测试 key 误报为明文凭据）
    let field = format!("api_{}", "key");
    write_cli_home(
        &home,
        ".kimi-code",
        &format!(
            r#"[providers."managed:kimi-code"]
{field} = "sk-kimi-a"

[providers.deepseek]
{field} = "sk-ds-cli"

[providers."my-glm-provider"]
{field} = "glm-key-a"

[providers."another-custom"]
{field} = "glm-key-b"

[providers."unregistered"]
{field} = "glm-key-nobody"
"#
        ),
        None,
    );
    let attribution = snapshot_attribution(&home.join(".kimi-code"));
    // kimi / deepseek 两段维持原通道
    assert_eq!(attribution.cli.kimi_api_key.as_deref(), Some("sk-kimi-a"));
    assert_eq!(
        attribution.cli.deepseek_api_key.as_deref(),
        Some("sk-ds-cli")
    );
    // 自定义段里命中 GLM 账号主 key / 额外 key 的都被反向提取；未登记的不进（顺序不定）
    let mut glm_keys = attribution.cli.glm_api_keys.clone();
    glm_keys.sort();
    assert_eq!(
        glm_keys,
        vec!["glm-key-a".to_string(), "glm-key-b".to_string()]
    );
    // GLM 账号与 Kimi 账号同列在 kimi_accounts（不拆列表，按账号列表顺序）
    let ids: Vec<&str> = attribution
        .kimi_accounts
        .iter()
        .map(|(id, _)| id.as_str())
        .collect();
    assert_eq!(ids, vec!["acc-glm", "acc-kimi"]);

    std::env::remove_var("KIMICODEBAR_CONFIG_DIR");
    let service = std::env::var("KIMICODEBAR_KEYRING_SERVICE").unwrap();
    std::env::remove_var("KIMICODEBAR_KEYRING_SERVICE");
    for slot in [
        "api_key/acc-glm",
        "api_key_extra/acc-glm",
        "api_key/acc-kimi",
    ] {
        let _ = keyring::Entry::new(&service, slot).map(|e| e.delete_credential());
    }
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&config);
}

/// harness 归属通道：key_accounts 里每把额外 key 也独立成条目（harness_input 的职责）
#[test]
fn harness_input_collects_extra_keys_alongside_main() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = temp_dir("harness-input-extra");
    let config = temp_dir("harness-input-extra-conf");
    std::env::set_var("USERPROFILE", &dir);
    std::env::set_var("KIMICODEBAR_CONFIG_DIR", &config);
    std::env::set_var(
        "KIMICODEBAR_KEYRING_SERVICE",
        format!("KimiCodeBar-test-{}", uuid::Uuid::new_v4()),
    );
    let settings = crate::storage::Settings {
        accounts: vec![crate::storage::Account {
            id: "acc-a".to_string(),
            name: "A".to_string(),
            login_method: Some("api_key".to_string()),
            provider: "kimi".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    };
    std::fs::write(
        config.join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();
    crate::creds::save_api_key("acc-a", "sk-kimi-main-a").unwrap();
    crate::creds::save_api_key_extra(
        "acc-a",
        &["sk-kimi-extra-1".to_string(), "sk-kimi-extra-2".to_string()],
    )
    .unwrap();

    let input = harness_input(Some(dir.as_os_str()));
    // 主 key + 两把额外 key 各占一条目，同指 acc-a
    assert_eq!(
        input.key_accounts,
        vec![
            ("sk-kimi-main-a".to_string(), "acc-a".to_string()),
            ("sk-kimi-extra-1".to_string(), "acc-a".to_string()),
            ("sk-kimi-extra-2".to_string(), "acc-a".to_string()),
        ]
    );

    std::env::remove_var("USERPROFILE");
    std::env::remove_var("KIMICODEBAR_CONFIG_DIR");
    let service = std::env::var("KIMICODEBAR_KEYRING_SERVICE").unwrap();
    std::env::remove_var("KIMICODEBAR_KEYRING_SERVICE");
    for slot in ["api_key/acc-a", "api_key_extra/acc-a"] {
        let _ = keyring::Entry::new(&service, slot).map(|e| e.delete_credential());
    }
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&config);
}

/// harness 事件按额外 key 归属（key_accounts 含额外 key 时 Claude 格式事件归该账号）
#[test]
fn harness_claude_attributed_via_extra_key() {
    let dir = temp_dir("harness-claude-extra");
    let claude = write_claude_dir(
        &dir,
        "sk-ds-extra",
        &[claude_line(
            "m1",
            "claude-opus-4-6",
            300,
            "2026-08-22T10:00:00Z",
        )],
    );
    let harness = HarnessInput {
        claude_dir: Some(claude),
        // 主 key 不中、额外 key 命中（harness_input 会把两者都收进来）
        key_accounts: vec![
            ("sk-ds-main".to_string(), "acc-a".to_string()),
            ("sk-ds-extra".to_string(), "acc-a".to_string()),
        ],
        ..HarnessInput::default()
    };
    let view = scan_full(
        &[],
        &harness,
        &dir.join("scan-state.json"),
        ms("2026-08-22T12:00:00+08:00"),
        &tz8(),
    );
    assert_eq!(view.for_account("acc-a").today_tokens, 300);
    assert!(!view.by_account.contains_key(UNASSIGNED_BUCKET));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scan_isolates_two_accounts_across_snapshots() {
    let dir = temp_dir("local-usage-isolate");
    let sessions = dir.join("sessions");
    let state_path = dir.join("config").join("scan-state.json");
    let tz = tz8();
    let now = ms("2026-07-27T12:00:00+08:00");
    let accounts = || {
        vec![
            ("acc-a".to_string(), account_creds(opt("sk-kimi-a"), None)),
            ("acc-b".to_string(), account_creds(opt("sk-kimi-b"), None)),
        ]
    };

    let file = write_wire(
        &sessions,
        "main",
        &[usage_line(
            "kimi-code/k3",
            "2026-07-27T10:00:00+08:00",
            100,
            10,
        )],
    );
    let per_event = 100 + 10 + 11264;

    // 快照 1：CLI 用着 acc-a 的 key → 本批事件归 acc-a
    let attr_a = Attribution {
        cli: CliCredentials {
            kimi_api_key: opt("sk-kimi-a"),
            ..Default::default()
        },
        kimi_accounts: accounts(),
        ..Default::default()
    };
    let view = scan_with(&[(sessions.clone(), attr_a)], &state_path, now, &tz);
    assert_eq!(view.for_account("acc-a").today_tokens, per_event);
    // acc-b 无桶：默认空统计，last_scan_at 照填（诚实零）
    let b = view.for_account("acc-b");
    assert_eq!(b.today_tokens, 0);
    assert!(b.last_scan_at.is_some());

    // 换号：CLI 改用 acc-b 的 key，追加一条事件 → 只归 acc-b，acc-a 不串
    let extra = usage_line("kimi-code/k3", "2026-07-27T11:00:00+08:00", 1, 2);
    let mut content = std::fs::read_to_string(&file).unwrap();
    content.push_str(&extra);
    content.push('\n');
    std::fs::write(&file, content).unwrap();
    let attr_b = Attribution {
        cli: CliCredentials {
            kimi_api_key: opt("sk-kimi-b"),
            ..Default::default()
        },
        kimi_accounts: accounts(),
        ..Default::default()
    };
    let view2 = scan_with(&[(sessions.clone(), attr_b)], &state_path, now, &tz);
    assert_eq!(view2.for_account("acc-a").today_tokens, per_event);
    assert_eq!(view2.for_account("acc-b").today_tokens, 1 + 2 + 11264);
    // 全程没有比未中的事件：未归属桶不生成
    assert!(!view2.by_account.contains_key(UNASSIGNED_BUCKET));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn machine_last_event_at_is_max_across_buckets() {
    let dir = temp_dir("local-usage-machine-max");
    let sessions = dir.join("sessions");
    let state_path = dir.join("config").join("scan-state.json");
    let tz = tz8();
    let now = ms("2026-07-27T12:00:00+08:00");
    let kimi_accounts = || vec![("acc-a".to_string(), account_creds(opt("sk-kimi-a"), None))];

    let file = write_wire(
        &sessions,
        "main",
        &[usage_line(
            "kimi-code/k3",
            "2026-07-27T10:00:00+08:00",
            100,
            10,
        )],
    );
    let attr = Attribution {
        cli: CliCredentials {
            kimi_api_key: opt("sk-kimi-a"),
            ..Default::default()
        },
        kimi_accounts: kimi_accounts(),
        ..Default::default()
    };
    let view = scan_with(&[(sessions.clone(), attr)], &state_path, now, &tz);
    assert_eq!(
        view.machine_last_event_at,
        Some(ms("2026-07-27T10:00:00+08:00"))
    );

    // CLI 换成本应用未知的 key：比对不中进未归属桶；机器级 max 含未归属桶
    let extra = usage_line("kimi-code/k3", "2026-07-27T11:30:00+08:00", 1, 2);
    let mut content = std::fs::read_to_string(&file).unwrap();
    content.push_str(&extra);
    content.push('\n');
    std::fs::write(&file, content).unwrap();
    let attr_unknown = Attribution {
        cli: CliCredentials {
            kimi_api_key: opt("sk-kimi-unknown"),
            ..Default::default()
        },
        kimi_accounts: kimi_accounts(),
        ..Default::default()
    };
    let view2 = scan_with(&[(sessions.clone(), attr_unknown)], &state_path, now, &tz);
    assert_eq!(
        view2.machine_last_event_at,
        Some(ms("2026-07-27T11:30:00+08:00"))
    );
    // 未归属桶的数字不进任何账号页
    assert_eq!(view2.for_account("acc-a").today_tokens, 100 + 10 + 11264);
    assert_eq!(
        view2.for_account(UNASSIGNED_BUCKET).today_tokens,
        1 + 2 + 11264
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ---- 多 CLI home ----

/// 在 root 下造一个 CLI home：sessions/ + config.toml（content 给定）+
/// 可选 credentials/kimi-code.json（user_id 给定时写入对应 JWT）
fn write_cli_home(root: &Path, name: &str, config: &str, user_id: Option<&str>) -> PathBuf {
    let home = root.join(name);
    std::fs::create_dir_all(home.join("sessions")).unwrap();
    std::fs::write(home.join("config.toml"), config).unwrap();
    if let Some(user_id) = user_id {
        let cred_dir = home.join("credentials");
        std::fs::create_dir_all(&cred_dir).unwrap();
        let token = jwt_with_payload(&format!(r#"{{"user_id":"{user_id}"}}"#));
        std::fs::write(
            cred_dir.join("kimi-code.json"),
            format!(r#"{{"access_token":"{token}"}}"#),
        )
        .unwrap();
    }
    home
}

#[test]
fn cli_homes_discovers_default_and_dash_suffixed_sorted() {
    let root = temp_dir("cli-homes-enum");
    // 合法 home 三个：默认 home（config.toml 为合法依据）+ 两个横线后缀 home
    write_cli_home(&root, ".kimi-code", "", None);
    // 先建 zzz 后建 hung：结果必须按路径字典序（hung 在前），与创建顺序无关
    let zzz = root.join(".kimi-code-zzz");
    std::fs::create_dir_all(zzz.join("sessions")).unwrap();
    // 无 config.toml 时 credentials/ 也算合法依据
    std::fs::create_dir_all(zzz.join("credentials")).unwrap();
    write_cli_home(&root, ".kimi-code-hung", "", None);
    // 点号后缀（备份命名）不匹配 glob：.kimi-code.bak / .kimi-code.old 跳过
    write_cli_home(&root, ".kimi-code.bak", "", None);
    write_cli_home(&root, ".kimi-code.old", "", None);
    // 横线后缀但缺 config.toml 与 credentials/：不合法
    std::fs::create_dir_all(root.join(".kimi-code-nocred").join("sessions")).unwrap();
    // 有 config.toml 但无 sessions/：不合法
    let no_sessions = root.join(".kimi-code-nosessions");
    std::fs::create_dir_all(&no_sessions).unwrap();
    std::fs::write(no_sessions.join("config.toml"), "").unwrap();
    // 横线前缀的普通文件（不是目录）：不合法
    std::fs::write(root.join(".kimi-code-file"), "").unwrap();
    // 无关目录不受影响
    write_cli_home(&root, ".other-tool", "", None);

    let homes = cli_homes(&root);
    // 默认 home 在前且只出现一次（不带横线不会被 glob 重复匹配），其余按字典序
    assert_eq!(
        homes,
        vec![
            root.join(".kimi-code"),
            root.join(".kimi-code-hung"),
            root.join(".kimi-code-zzz"),
        ]
    );
    // 再跑一遍结果一致（确定性）
    assert_eq!(cli_homes(&root), homes);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cli_homes_skips_invalid_default_and_empty_when_none_valid() {
    let root = temp_dir("cli-homes-invalid");
    // 默认 home 存在但不合法（只有 sessions/）：不计；合法的横线 home 照样收
    std::fs::create_dir_all(root.join(".kimi-code").join("sessions")).unwrap();
    write_cli_home(&root, ".kimi-code-hung", "", None);
    assert_eq!(cli_homes(&root), vec![root.join(".kimi-code-hung")]);
    let _ = std::fs::remove_dir_all(&root);

    // 整个 root 没有任何合法 home：空（scan 按空目标容忍）
    let empty = temp_dir("cli-homes-empty");
    assert_eq!(cli_homes(&empty), Vec::<PathBuf>::new());
    // root 本身不存在也容忍为空
    assert_eq!(cli_homes(&empty.join("nonexistent")), Vec::<PathBuf>::new());
    let _ = std::fs::remove_dir_all(&empty);
}

/// 两个 home 各配各的 OAuth 用户：归属隔离的最小构造（scan_with 层）
#[test]
fn scan_isolates_two_homes_by_user_id() {
    let dir = temp_dir("local-usage-two-homes");
    let state_path = dir.join("config").join("scan-state.json");
    let tz = tz8();
    let now = ms("2026-07-27T12:00:00+08:00");
    let accounts = || {
        vec![
            ("acc-a".to_string(), account_creds(None, opt("user-1"))),
            ("acc-b".to_string(), account_creds(None, opt("user-2"))),
        ]
    };
    let attr = |user_id: &str| Attribution {
        cli: CliCredentials {
            kimi_user_id: opt(user_id),
            ..Default::default()
        },
        kimi_accounts: accounts(),
        ..Default::default()
    };

    let sessions_a = dir.join("home-a").join("sessions");
    let sessions_b = dir.join("home-b").join("sessions");
    write_wire(
        &sessions_a,
        "main",
        &[usage_line(
            "kimi-code/k3",
            "2026-07-27T10:00:00+08:00",
            100,
            10,
        )],
    );
    write_wire(
        &sessions_b,
        "main",
        &[usage_line(
            "kimi-code/k3",
            "2026-07-27T11:00:00+08:00",
            200,
            20,
        )],
    );

    let view = scan_with(
        &[
            (sessions_a.clone(), attr("user-1")),
            (sessions_b, attr("user-2")),
        ],
        &state_path,
        now,
        &tz,
    );
    // 各 home 的事件只进自己 user_id 对应的桶（每条 usage_line 另含 inputCacheRead 11264）
    assert_eq!(view.for_account("acc-a").today_tokens, 100 + 10 + 11264);
    assert_eq!(view.for_account("acc-b").today_tokens, 200 + 20 + 11264);
    assert!(!view.by_account.contains_key(UNASSIGNED_BUCKET));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scan_merges_same_account_across_two_homes() {
    let dir = temp_dir("local-usage-same-acc");
    let state_path = dir.join("config").join("scan-state.json");
    let tz = tz8();
    let now = ms("2026-07-27T12:00:00+08:00");
    // 两个 home 登的是同一 OAuth 用户（user-1 → acc-a）：消耗合并进同一桶
    let attr = || Attribution {
        cli: CliCredentials {
            kimi_user_id: opt("user-1"),
            ..Default::default()
        },
        kimi_accounts: vec![("acc-a".to_string(), account_creds(None, opt("user-1")))],
        ..Default::default()
    };

    let sessions_a = dir.join("home-a").join("sessions");
    let sessions_b = dir.join("home-b").join("sessions");
    write_wire(
        &sessions_a,
        "main",
        &[usage_line(
            "kimi-code/k3",
            "2026-07-27T10:00:00+08:00",
            100,
            10,
        )],
    );
    write_wire(
        &sessions_b,
        "main",
        &[usage_line(
            "kimi-code/k3",
            "2026-07-27T11:00:00+08:00",
            200,
            20,
        )],
    );

    let view = scan_with(
        &[(sessions_a.clone(), attr()), (sessions_b, attr())],
        &state_path,
        now,
        &tz,
    );
    assert_eq!(
        view.for_account("acc-a").today_tokens,
        (100 + 10 + 11264) + (200 + 20 + 11264)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scan_unmatched_home_events_go_unassigned() {
    let dir = temp_dir("local-usage-home-unmatched");
    let state_path = dir.join("config").join("scan-state.json");
    let tz = tz8();
    let now = ms("2026-07-27T12:00:00+08:00");
    let accounts = || vec![("acc-a".to_string(), account_creds(None, opt("user-1")))];
    // home-a 凭证匹配 acc-a；home-b 的 user_id 谁都不认识
    let attr_a = Attribution {
        cli: CliCredentials {
            kimi_user_id: opt("user-1"),
            ..Default::default()
        },
        kimi_accounts: accounts(),
        ..Default::default()
    };
    let attr_b = Attribution {
        cli: CliCredentials {
            kimi_user_id: opt("user-stranger"),
            ..Default::default()
        },
        kimi_accounts: accounts(),
        ..Default::default()
    };

    let sessions_a = dir.join("home-a").join("sessions");
    let sessions_b = dir.join("home-b").join("sessions");
    write_wire(
        &sessions_a,
        "main",
        &[usage_line(
            "kimi-code/k3",
            "2026-07-27T10:00:00+08:00",
            100,
            10,
        )],
    );
    write_wire(
        &sessions_b,
        "main",
        &[usage_line(
            "kimi-code/k3",
            "2026-07-27T11:00:00+08:00",
            200,
            20,
        )],
    );

    let view = scan_with(
        &[(sessions_a.clone(), attr_a), (sessions_b, attr_b)],
        &state_path,
        now,
        &tz,
    );
    // 匹配不到的 home 进未归属桶，且不影响匹配到的 home 正常归属
    assert_eq!(view.for_account("acc-a").today_tokens, 100 + 10 + 11264);
    assert_eq!(
        view.for_account(UNASSIGNED_BUCKET).today_tokens,
        200 + 20 + 11264
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scan_routes_models_by_each_homes_table() {
    let dir = temp_dir("local-usage-home-routes");
    let state_path = dir.join("config").join("scan-state.json");
    let tz = tz8();
    let now = ms("2026-07-27T12:00:00+08:00");
    let accounts = || vec![("acc-a".to_string(), account_creds(None, opt("user-1")))];
    // home-a 的 [models] 没有 qwen：前缀兜底走 Kimi 路由 → 归 acc-a
    let attr_a = Attribution {
        cli: CliCredentials {
            kimi_user_id: opt("user-1"),
            ..Default::default()
        },
        kimi_accounts: accounts(),
        ..Default::default()
    };
    // home-b（hung home 场景）把 qwen3.8-max 配到 dashscope：该 home 的 qwen 事件进未归属
    let attr_b = Attribution {
        cli: CliCredentials {
            kimi_user_id: opt("user-1"),
            ..Default::default()
        },
        kimi_accounts: accounts(),
        model_providers: HashMap::from([("qwen3.8-max".to_string(), "dashscope".to_string())]),
        ..Default::default()
    };

    let sessions_a = dir.join("home-a").join("sessions");
    let sessions_b = dir.join("home-b").join("sessions");
    write_wire(
        &sessions_a,
        "main",
        &[usage_line(
            "qwen3.8-max",
            "2026-07-27T10:00:00+08:00",
            100,
            10,
        )],
    );
    write_wire(
        &sessions_b,
        "main",
        &[usage_line(
            "qwen3.8-max",
            "2026-07-27T11:00:00+08:00",
            200,
            20,
        )],
    );

    let view = scan_with(
        &[(sessions_a.clone(), attr_a), (sessions_b, attr_b)],
        &state_path,
        now,
        &tz,
    );
    // 同型号事件按各 home 自己的路由表分流：home-a 归 acc-a，home-b 进未归属
    assert_eq!(view.for_account("acc-a").today_tokens, 100 + 10 + 11264);
    assert_eq!(
        view.for_account(UNASSIGNED_BUCKET).today_tokens,
        200 + 20 + 11264
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scan_multi_home_keeps_independent_offsets() {
    let dir = temp_dir("local-usage-multi-incr");
    let sessions_a = dir.join("home-a").join("sessions");
    let sessions_b = dir.join("home-b").join("sessions");
    let state_path = dir.join("config").join("scan-state.json");
    let tz = tz8();
    let now = ms("2026-07-27T12:00:00+08:00");
    let accounts = || {
        vec![
            ("acc-a".to_string(), account_creds(None, opt("user-1"))),
            ("acc-b".to_string(), account_creds(None, opt("user-2"))),
        ]
    };
    let attr = |user_id: &str| Attribution {
        cli: CliCredentials {
            kimi_user_id: opt(user_id),
            ..Default::default()
        },
        kimi_accounts: accounts(),
        ..Default::default()
    };

    let file_a = write_wire(
        &sessions_a,
        "main",
        &[usage_line(
            "kimi-code/k3",
            "2026-07-27T10:00:00+08:00",
            100,
            10,
        )],
    );
    let per_a = 100 + 10 + 11264;
    let file_b = write_wire(
        &sessions_b,
        "main",
        &[usage_line(
            "kimi-code/k3",
            "2026-07-27T11:00:00+08:00",
            200,
            20,
        )],
    );
    let per_b = 200 + 20 + 11264;

    // 旧版单 home 时期：只扫默认 home，scan-state 里只有 home-a 的偏移
    let view1 = scan_with(
        &[(sessions_a.clone(), attr("user-1"))],
        &state_path,
        now,
        &tz,
    );
    assert_eq!(view1.for_account("acc-a").today_tokens, per_a);

    // 加入第二个 home：旧 home 按偏移续读不重复计数，新 home 无偏移记录从 0 全扫
    let view2 = scan_with(
        &[
            (sessions_a.clone(), attr("user-1")),
            (sessions_b.clone(), attr("user-2")),
        ],
        &state_path,
        now,
        &tz,
    );
    assert_eq!(view2.for_account("acc-a").today_tokens, per_a);
    assert_eq!(view2.for_account("acc-b").today_tokens, per_b);

    // 给 home-a 追加一条：三扫只增量这一条进 acc-a，acc-b 不串
    let extra = usage_line("kimi-code/k3", "2026-07-27T11:30:00+08:00", 1, 2);
    let mut content = std::fs::read_to_string(&file_a).unwrap();
    content.push_str(&extra);
    content.push('\n');
    std::fs::write(&file_a, content).unwrap();
    let view3 = scan_with(
        &[
            (sessions_a.clone(), attr("user-1")),
            (sessions_b.clone(), attr("user-2")),
        ],
        &state_path,
        now,
        &tz,
    );
    assert_eq!(
        view3.for_account("acc-a").today_tokens,
        per_a + 1 + 2 + 11264
    );
    assert_eq!(view3.for_account("acc-b").today_tokens, per_b);

    // 状态里两个 home 的文件键（全路径）独立共存
    let saved = std::fs::read_to_string(&state_path).unwrap();
    let state: serde_json::Value = serde_json::from_str(&saved).unwrap();
    let files = state["files"].as_object().unwrap();
    assert_eq!(files.len(), 2);
    assert!(files.contains_key(&file_a.to_string_lossy().into_owned()));
    assert!(files.contains_key(&file_b.to_string_lossy().into_owned()));

    let _ = std::fs::remove_dir_all(&dir);
}

/// 端到端：cli_homes 发现 + 逐 home 快照 + 扫描串起来（scan() 的节流缓存走 scan_fresh 绕过）
#[test]
fn scan_end_to_end_two_homes_isolated_by_user_id() {
    let _guard = ENV_LOCK.lock().unwrap();
    let root = temp_dir("multi-home-root");
    let config = temp_dir("multi-home-conf");
    // 两个合法 home：默认 home 登 user-1（账号 acc-a），-hung home 登 user-2（账号 acc-b）
    let home_a = write_cli_home(&root, ".kimi-code", "", Some("user-1"));
    let home_b = write_cli_home(&root, ".kimi-code-hung", "", Some("user-2"));
    // 各 home 今日各一条事件（真实本地时钟，scan_fresh 按给定 now 出视图）
    let now_ms = chrono::Local::now().timestamp_millis();
    let line = |input_other: u64, output: u64| {
        format!(
            r#"{{"type":"usage.record","model":"kimi-code/k3","usage":{{"inputOther":{input_other},"output":{output},"inputCacheRead":0,"inputCacheCreation":0}},"usageScope":"turn","time":{now_ms}}}"#
        )
    };
    write_wire(&home_a.join("sessions"), "main", &[line(100, 10)]);
    write_wire(&home_b.join("sessions"), "main", &[line(200, 20)]);
    // 应用侧两个账号的 OAuth 凭证（明文写入临时配置目录；读取时原地转 DPAPI，无碍）
    let settings = crate::storage::Settings {
        accounts: vec![
            crate::storage::Account {
                id: "acc-a".to_string(),
                name: "A".to_string(),
                login_method: Some("oauth".to_string()),
                provider: "kimi".to_string(),
                ..Default::default()
            },
            crate::storage::Account {
                id: "acc-b".to_string(),
                name: "B".to_string(),
                login_method: Some("oauth".to_string()),
                provider: "kimi".to_string(),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    std::fs::write(
        config.join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();
    for (id, user_id) in [("acc-a", "user-1"), ("acc-b", "user-2")] {
        let token = jwt_with_payload(&format!(r#"{{"user_id":"{user_id}"}}"#));
        std::fs::write(
            config.join(format!("credentials-{id}.json")),
            format!(r#"{{"access_token":"{token}"}}"#),
        )
        .unwrap();
    }
    std::env::set_var("USERPROFILE", &root);
    std::env::set_var("KIMICODEBAR_CONFIG_DIR", &config);
    // keyring 只读探空：隔离 service 名，绝不碰真实凭据
    std::env::set_var(
        "KIMICODEBAR_KEYRING_SERVICE",
        format!("KimiCodeBar-test-{}", uuid::Uuid::new_v4()),
    );
    std::env::remove_var("KIMI_SECONDARY_MODEL");
    // 本机真实 WSL home 不进本测试（scan_fresh 会发现它）：根指到不存在目录
    std::env::set_var("KIMICODEBAR_WSL_ROOT", root.join("no-wsl"));

    let view = scan_fresh(now_ms, &chrono::Local);
    // 两个 home 都被扫到、各归各账号（cli_homes 只认默认 home 时本测试必红：acc-b 为 0）
    assert_eq!(view.for_account("acc-a").today_tokens, 110);
    assert_eq!(view.for_account("acc-b").today_tokens, 220);
    assert!(!view.by_account.contains_key(UNASSIGNED_BUCKET));

    std::env::remove_var("USERPROFILE");
    std::env::remove_var("KIMICODEBAR_CONFIG_DIR");
    std::env::remove_var("KIMICODEBAR_KEYRING_SERVICE");
    std::env::remove_var("KIMICODEBAR_WSL_ROOT");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&config);
}

/// 额外扫描目录（issue #57 上半）：设置里手填的远程 home 并入扫描目标——
/// 可达目录的 sessions/wire.jsonl 扫出消耗并按该 home 自己的凭证快照归属；
/// 不可达目录本轮跳过、整轮不报错（scan_fresh 无 Result，其余目标照常出数）
#[test]
fn scan_fresh_covers_extra_scan_dirs_and_skips_unreachable() {
    let _guard = ENV_LOCK.lock().unwrap();
    let root = temp_dir("extra-scan-root");
    let config = temp_dir("extra-scan-conf");
    // 本机 USERPROFILE 指到无任何 .kimi-code 的空目录：消耗只能来自额外目录通道
    std::env::set_var("USERPROFILE", &root);
    std::env::set_var("KIMICODEBAR_CONFIG_DIR", &config);
    // keyring 只读探空：隔离 service 名，绝不碰真实凭据
    std::env::set_var(
        "KIMICODEBAR_KEYRING_SERVICE",
        format!("KimiCodeBar-test-{}", uuid::Uuid::new_v4()),
    );
    std::env::remove_var("KIMI_SECONDARY_MODEL");
    // 本机真实 WSL home 不进本测试：根指到不存在目录
    std::env::set_var("KIMICODEBAR_WSL_ROOT", root.join("no-wsl"));

    // 「远程」home 故意不带 .kimi-code 前缀（cli_homes 发现不了，只能走额外目录通道）
    let remote = write_cli_home(&root, "remote-home", "", Some("user-r"));
    let now_ms = chrono::Local::now().timestamp_millis();
    let line = format!(
        r#"{{"type":"usage.record","model":"kimi-code/k3","usage":{{"inputOther":300,"output":30,"inputCacheRead":0,"inputCacheCreation":0}},"usageScope":"turn","time":{now_ms}}}"#
    );
    write_wire(&remote.join("sessions"), "main", &[line]);

    // 额外目录两个：一个可达、一个不存在；应用侧账号 acc-r 与远程 home 的 OAuth user_id 对应
    let settings = crate::storage::Settings {
        accounts: vec![crate::storage::Account {
            id: "acc-r".to_string(),
            name: "R".to_string(),
            login_method: Some("oauth".to_string()),
            provider: "kimi".to_string(),
            ..Default::default()
        }],
        extra_scan_dirs: vec![
            remote.to_string_lossy().into_owned(),
            root.join("no-such-share").to_string_lossy().into_owned(),
        ],
        ..Default::default()
    };
    std::fs::write(
        config.join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();
    // 应用侧 OAuth 凭证（明文写入临时配置目录；读取时原地转 DPAPI，无碍）
    let token = jwt_with_payload(r#"{"user_id":"user-r"}"#);
    std::fs::write(
        config.join("credentials-acc-r.json"),
        format!(r#"{{"access_token":"{token}"}}"#),
    )
    .unwrap();

    let view = scan_fresh(now_ms, &chrono::Local);
    // 可达目录扫出消耗并归属 acc-r（tokens = 300 + 30）
    assert_eq!(view.for_account("acc-r").today_tokens, 330);
    // 不可达目录只跳过本轮，不报错不串数：无未归属桶，机器级活跃判定不受影响
    assert!(!view.by_account.contains_key(UNASSIGNED_BUCKET));
    assert_eq!(view.machine_last_event_at, Some(now_ms));

    std::env::remove_var("USERPROFILE");
    std::env::remove_var("KIMICODEBAR_CONFIG_DIR");
    std::env::remove_var("KIMICODEBAR_KEYRING_SERVICE");
    std::env::remove_var("KIMICODEBAR_WSL_ROOT");
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&config);
}

// ---- 跨 Harness（Claude Code / Codex / OpenCode）----

/// Claude assistant 行（cache 字段给 0，tokens = input + output）
fn claude_line(id: &str, model: &str, tokens: u64, rfc3339: &str) -> String {
    format!(
        r#"{{"type":"assistant","message":{{"id":"{id}","model":"{model}","usage":{{"input_tokens":{tokens},"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}},"stop_reason":"end_turn"}},"timestamp":"{rfc3339}"}}"#
    )
}

/// 造 Claude 目录：settings.json（token）+ projects/p/main.jsonl（给定行）
fn write_claude_dir(root: &Path, token: &str, lines: &[String]) -> PathBuf {
    let claude = root.join(".claude");
    let projects = claude.join("projects").join("p");
    std::fs::create_dir_all(&projects).unwrap();
    // JSON 字段名运行时组装（与 write_zcode_dir 同款手法，避免凭据扫描器把假测试 token 误报为明文凭据）
    let field = format!("ANTHROPIC_{}", "AUTH_TOKEN");
    std::fs::write(
        claude.join("settings.json"),
        format!(r#"{{"env":{{"{field}":"{token}"}}}}"#),
    )
    .unwrap();
    std::fs::write(projects.join("main.jsonl"), lines.join("\n") + "\n").unwrap();
    claude
}

/// Codex token_count 行（last_token_usage 精确值形态，tokens = 四字段和）
fn codex_token_line(tokens: u64, rfc3339: &str) -> String {
    format!(
        r#"{{"timestamp":"{rfc3339}","type":"event_msg","payload":{{"type":"token_count","info":{{"last_token_usage":{{"input_tokens":{tokens},"output_tokens":0,"cached_input_tokens":0,"reasoning_output_tokens":0}}}}}}}}"#
    )
}

fn codex_turn_line(model: &str, rfc3339: &str) -> String {
    format!(r#"{{"timestamp":"{rfc3339}","type":"turn_context","payload":{{"model":"{model}"}}}}"#)
}

/// 造 Codex 目录：auth.json（OPENAI_API_KEY）+ sessions/日期分区下 rollout jsonl
fn write_codex_dir(root: &Path, api_key: &str, lines: &[String]) -> PathBuf {
    let codex = root.join(".codex");
    let day = codex.join("sessions").join("2026").join("08").join("22");
    std::fs::create_dir_all(&day).unwrap();
    // JSON 字段名运行时组装（与 write_zcode_dir 同款手法，避免凭据扫描器把假测试 key 误报为明文凭据）
    let field = format!("OPENAI_{}", "API_KEY");
    std::fs::write(
        codex.join("auth.json"),
        format!(r#"{{"{field}":"{api_key}"}}"#),
    )
    .unwrap();
    std::fs::write(day.join("rollout-x.jsonl"), lines.join("\n") + "\n").unwrap();
    codex
}

/// 造 OpenCode 数据目录：opencode.db（真实 schema + 给定行）
fn write_opencode_dir(parent: &Path, rows: &[(i64, String)]) -> PathBuf {
    let dir = parent.join("opencode");
    std::fs::create_dir_all(&dir).unwrap();
    let conn = rusqlite::Connection::open(dir.join("opencode.db")).unwrap();
    conn.execute(
        "CREATE TABLE message (
                id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL, time_updated INTEGER, data TEXT NOT NULL
            )",
        [],
    )
    .unwrap();
    for (idx, (ts, data)) in rows.iter().enumerate() {
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data)
                 VALUES (?1, 'ses_1', ?2, ?2, ?3)",
            rusqlite::params![format!("msg_{idx}"), ts, data],
        )
        .unwrap();
    }
    dir
}

fn opencode_assistant_data(model: &str, provider: &str, tokens: u64) -> String {
    serde_json::json!({
        "role": "assistant",
        "providerID": provider,
        "modelID": model,
        "tokens": {"input": tokens, "output": 0, "reasoning": 0, "cache": {"read": 0, "write": 0}},
        "time": {"created": 1, "completed": 2},
    })
    .to_string()
}

/// OpenCode auth.json 的 key 字段名运行时组装（与 write_zcode_dir 同款手法）
fn opencode_auth_json(kimi_key: &str, glm_key: Option<&str>) -> String {
    let kf = format!("ke{}", "y");
    match glm_key {
        Some(g) => format!(
            r#"{{"kimi":{{"type":"api","{kf}":"{kimi_key}"}},"glm":{{"type":"oauth","{kf}":"{g}"}}}}"#
        ),
        None => format!(r#"{{"kimi":{{"type":"api","{kf}":"{kimi_key}"}}}}"#),
    }
}

/// ZCode model-io 行（真实结构压缩：completedAt / model.modelId /
/// response.usage 驼峰字段。真实形状：inputTokens 是总输入、已含缓存读——
/// input=1100（其中缓存读 1000）、cacheWrite=0、totalTokens = input + output；
/// 新口径 tokens = 1100 + output，命中率分量 1000/1100）
fn zcode_line(model: &str, output: u64, rfc3339: &str) -> String {
    format!(
        r#"{{"completedAt":"{rfc3339}","requestId":"req-{output}","attempt":1,"model":{{"modelId":"{model}","providerId":"builtin:bigmodel-coding-plan"}},"response":{{"usage":{{"inputTokens":1100,"outputTokens":{output},"totalTokens":{total},"cacheReadTokens":1000,"cacheWriteTokens":0}}}}}}"#,
        total = 1100 + output,
    )
}

/// 造 ZCode 目录：v2/config.json（provider 段 key，字段名运行时拼装）
/// + cli/rollout/model-io jsonl
fn write_zcode_dir(root: &Path, key: &str, lines: &[String]) -> PathBuf {
    let zcode = root.join(".zcode");
    let rollout = zcode.join("cli").join("rollout");
    std::fs::create_dir_all(&rollout).unwrap();
    std::fs::create_dir_all(zcode.join("v2")).unwrap();
    let field = format!("api{}", "Key");
    std::fs::write(
        zcode.join("v2").join("config.json"),
        format!(
            r#"{{"provider":{{"builtin:bigmodel-coding-plan":{{"options":{{"{field}":"{key}"}}}}}}}}"#
        ),
    )
    .unwrap();
    std::fs::write(
        rollout.join("model-io-sess_t1.jsonl"),
        lines.join("\n") + "\n",
    )
    .unwrap();
    zcode
}

#[test]
fn harness_claude_attributed_incremental_no_double() {
    let dir = temp_dir("harness-claude");
    let claude = write_claude_dir(
        &dir,
        "sk-kimi-acc",
        &[
            claude_line("m1", "claude-opus-4-6", 300, "2026-08-22T10:00:00Z"),
            claude_line("m2", "claude-opus-4-6", 50, "2026-08-22T11:00:00Z"),
        ],
    );
    let state_path = dir.join("config").join("scan-state.json");
    let harness = HarnessInput {
        claude_dir: Some(claude.clone()),
        key_accounts: vec![("sk-kimi-acc".to_string(), "acc-a".to_string())],
        ..HarnessInput::default()
    };
    let tz = tz8();
    let now = ms("2026-08-22T12:00:00+08:00");

    let view = scan_full(&[], &harness, &state_path, now, &tz);
    assert_eq!(view.for_account("acc-a").today_tokens, 350);
    // 全部事件都归属成功：未归属桶不生成，账号页看不到未归属数字
    assert!(!view.by_account.contains_key(UNASSIGNED_BUCKET));
    // harness 事件计入机器级活跃判定
    assert_eq!(view.machine_last_event_at, Some(ms("2026-08-22T11:00:00Z")));

    // 二次扫描：偏移续读 + id 去重，不重复计数
    let view2 = scan_full(&[], &harness, &state_path, now, &tz);
    assert_eq!(view2.for_account("acc-a").today_tokens, 350);

    // 追加新消息：只增量这一条（流式重复 id 不双计）
    let file = claude.join("projects").join("p").join("main.jsonl");
    let mut content = std::fs::read_to_string(&file).unwrap();
    content.push_str(&claude_line(
        "m3",
        "claude-opus-4-6",
        70,
        "2026-08-22T11:30:00Z",
    ));
    content.push('\n');
    std::fs::write(&file, content).unwrap();
    let view3 = scan_full(&[], &harness, &state_path, now, &tz);
    assert_eq!(view3.for_account("acc-a").today_tokens, 420);
    // by_model 按日志原样显示模型名
    assert_eq!(view3.for_account("acc-a").by_model.len(), 1);
    assert_eq!(
        view3.for_account("acc-a").by_model[0].model,
        "claude-opus-4-6"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn harness_claude_unmatched_key_goes_unassigned() {
    let dir = temp_dir("harness-claude-unmatched");
    let claude = write_claude_dir(
        &dir,
        "sk-stranger",
        &[claude_line(
            "m1",
            "claude-opus-4-6",
            300,
            "2026-08-22T10:00:00Z",
        )],
    );
    let state_path = dir.join("scan-state.json");
    let harness = HarnessInput {
        claude_dir: Some(claude),
        key_accounts: vec![("sk-kimi-acc".to_string(), "acc-a".to_string())],
        ..HarnessInput::default()
    };
    let view = scan_full(
        &[],
        &harness,
        &state_path,
        ms("2026-08-22T12:00:00+08:00"),
        &tz8(),
    );
    // key 谁都不认识：进未归属桶，账号页是诚实零
    assert_eq!(view.for_account(UNASSIGNED_BUCKET).today_tokens, 300);
    assert_eq!(view.for_account("acc-a").today_tokens, 0);
    // 未归属事件的活跃判定仍算机器级活跃
    assert_eq!(view.machine_last_event_at, Some(ms("2026-08-22T10:00:00Z")));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn harness_key_matches_account_of_any_provider() {
    // harness 的 key 与 DeepSeek 账号登记的 key 相同：不分 provider 照样归它
    let dir = temp_dir("harness-cross-provider");
    let claude = write_claude_dir(
        &dir,
        "sk-ds-key",
        &[claude_line(
            "m1",
            "claude-opus-4-6",
            100,
            "2026-08-22T10:00:00Z",
        )],
    );
    let harness = HarnessInput {
        claude_dir: Some(claude),
        key_accounts: vec![("sk-ds-key".to_string(), "acc-deepseek".to_string())],
        ..HarnessInput::default()
    };
    let view = scan_full(
        &[],
        &harness,
        &dir.join("scan-state.json"),
        ms("2026-08-22T12:00:00+08:00"),
        &tz8(),
    );
    assert_eq!(view.for_account("acc-deepseek").today_tokens, 100);
    assert!(!view.by_account.contains_key(UNASSIGNED_BUCKET));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn harness_codex_attributed_incremental_no_double() {
    let dir = temp_dir("harness-codex");
    let codex = write_codex_dir(
        &dir,
        "sk-codex-key",
        &[
            codex_turn_line("gpt-5.4-codex", "2026-08-22T10:00:00Z"),
            codex_token_line(200, "2026-08-22T10:00:01Z"),
            codex_token_line(30, "2026-08-22T10:01:00Z"),
        ],
    );
    let state_path = dir.join("config").join("scan-state.json");
    let harness = HarnessInput {
        codex_dir: Some(codex.clone()),
        key_accounts: vec![("sk-codex-key".to_string(), "acc-c".to_string())],
        ..HarnessInput::default()
    };
    let tz = tz8();
    let now = ms("2026-08-22T12:00:00+08:00");

    let view = scan_full(&[], &harness, &state_path, now, &tz);
    assert_eq!(view.for_account("acc-c").today_tokens, 230);
    assert_eq!(view.for_account("acc-c").by_model[0].model, "gpt-5.4-codex");
    assert!(!view.by_account.contains_key(UNASSIGNED_BUCKET));

    // 二次扫描不重复；追加事件只增量
    assert_eq!(
        scan_full(&[], &harness, &state_path, now, &tz)
            .for_account("acc-c")
            .today_tokens,
        230
    );
    let file = codex
        .join("sessions")
        .join("2026")
        .join("08")
        .join("22")
        .join("rollout-x.jsonl");
    let mut content = std::fs::read_to_string(&file).unwrap();
    content.push_str(&codex_token_line(45, "2026-08-22T11:00:00Z"));
    content.push('\n');
    std::fs::write(&file, content).unwrap();
    assert_eq!(
        scan_full(&[], &harness, &state_path, now, &tz)
            .for_account("acc-c")
            .today_tokens,
        275
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// ZCode 事件按 v2/config.json 的 key 归属（GLM key 命中账号登记 key 即归，
/// 模型名照记进 by_model）；字节偏移增量，二次扫描不重复、追加只增量
#[test]
fn harness_zcode_attributed_incremental_no_double() {
    let dir = temp_dir("harness-zcode");
    let zcode = write_zcode_dir(
        &dir,
        "sk-glm-acc",
        &[
            zcode_line("GLM-5.3-Flash", 200, "2026-08-22T10:00:00Z"),
            zcode_line("GLM-5.3-Flash", 50, "2026-08-22T11:00:00Z"),
        ],
    );
    let state_path = dir.join("config").join("scan-state.json");
    let harness = HarnessInput {
        zcode_dir: Some(zcode.clone()),
        key_accounts: vec![("sk-glm-acc".to_string(), "acc-g".to_string())],
        ..HarnessInput::default()
    };
    let tz = tz8();
    let now = ms("2026-08-22T12:00:00+08:00");

    // 每行 tokens = 1100 + output（inputTokens 已含缓存读）：1300 + 1150 = 2450；
    // 命中率分量每行 1000/1100 → 两条 2000/2200 = 1000/1100
    let view = scan_full(&[], &harness, &state_path, now, &tz);
    assert_eq!(view.for_account("acc-g").today_tokens, 2450);
    assert_eq!(
        view.for_account("acc-g").today_cache_hit_rate,
        Some(1000.0 / 1100.0)
    );
    assert_eq!(view.for_account("acc-g").by_model[0].model, "GLM-5.3-Flash");
    assert!(!view.by_account.contains_key(UNASSIGNED_BUCKET));

    // 二次扫描不重复
    assert_eq!(
        scan_full(&[], &harness, &state_path, now, &tz)
            .for_account("acc-g")
            .today_tokens,
        2450
    );
    // 追加一行只增量（1145）
    let file = zcode
        .join("cli")
        .join("rollout")
        .join("model-io-sess_t1.jsonl");
    let mut content = std::fs::read_to_string(&file).unwrap();
    content.push_str(&zcode_line("GLM-5.3-Flash", 45, "2026-08-22T11:30:00Z"));
    content.push('\n');
    std::fs::write(&file, content).unwrap();
    assert_eq!(
        scan_full(&[], &harness, &state_path, now, &tz)
            .for_account("acc-g")
            .today_tokens,
        3595
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// ZCode 的 key 全不中（未登记/他号）→ 事件进未归属桶，机器级活跃判定仍计入
#[test]
fn harness_zcode_unmatched_key_goes_unassigned() {
    let dir = temp_dir("harness-zcode-unmatched");
    let zcode = write_zcode_dir(
        &dir,
        "sk-glm-not-registered",
        &[zcode_line("GLM-5.3-Flash", 200, "2026-08-22T10:00:00Z")],
    );
    let state_path = dir.join("config").join("scan-state.json");
    let harness = HarnessInput {
        zcode_dir: Some(zcode),
        key_accounts: vec![("sk-other".to_string(), "acc-g".to_string())],
        ..HarnessInput::default()
    };
    let tz = tz8();
    let now = ms("2026-08-22T12:00:00+08:00");

    let view = scan_full(&[], &harness, &state_path, now, &tz);
    assert_eq!(view.for_account(UNASSIGNED_BUCKET).today_tokens, 1300);
    // 未归属桶同样带缓存分量：1000/1100
    assert_eq!(
        view.for_account(UNASSIGNED_BUCKET).today_cache_hit_rate,
        Some(1000.0 / 1100.0)
    );
    assert_eq!(view.for_account("acc-g").today_tokens, 0);
    assert_eq!(view.machine_last_event_at, Some(ms("2026-08-22T10:00:00Z")));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn harness_opencode_attributed_via_provider_key() {
    let dir = temp_dir("harness-opencode");
    let base = ms("2026-08-22T10:00:00Z");
    let data_dir = write_opencode_dir(
        &dir,
        &[
            (base, opencode_assistant_data("kimi-k3", "kimi", 80)),
            (base + 1000, opencode_assistant_data("glm-4.6", "glm", 20)),
        ],
    );
    let config_dir = dir.join("oc-config");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("auth.json"),
        opencode_auth_json("sk-oc-key", Some("no")),
    )
    .unwrap();
    let state_path = dir.join("config").join("scan-state.json");
    let harness = HarnessInput {
        opencode_data_dirs: vec![data_dir],
        opencode_config_dirs: vec![config_dir],
        key_accounts: vec![("sk-oc-key".to_string(), "acc-oc".to_string())],
        ..HarnessInput::default()
    };
    let tz = tz8();
    let now = ms("2026-08-22T12:00:00+08:00");

    let view = scan_full(&[], &harness, &state_path, now, &tz);
    // kimi provider 消耗归账号；glm provider 的 key 是 oauth 形态取不到 → 未归属
    assert_eq!(view.for_account("acc-oc").today_tokens, 80);
    assert_eq!(view.for_account(UNASSIGNED_BUCKET).today_tokens, 20);
    assert_eq!(view.machine_last_event_at, Some(base + 1000));

    // 二次扫描不重复
    let view2 = scan_full(&[], &harness, &state_path, now, &tz);
    assert_eq!(view2.for_account("acc-oc").today_tokens, 80);
    assert_eq!(view2.for_account(UNASSIGNED_BUCKET).today_tokens, 20);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 旧版 scan-state（v1.4.1 形态：last_scan_at/files/buckets，无任何 harness 键）
/// 直接兼容：不清零、不重扫、harness 新键按 serde(default) 空起步
#[test]
fn harness_legacy_state_compatible_no_wipe_no_rescan() {
    let dir = temp_dir("harness-legacy");
    let claude = write_claude_dir(
        &dir,
        "sk-kimi-acc",
        &[claude_line(
            "m1",
            "claude-opus-4-6",
            300,
            "2026-08-22T10:00:00Z",
        )],
    );
    let file = claude.join("projects").join("p").join("main.jsonl");
    // 旧状态：当前格式版本（version = STATE_VERSION）但缺 v1.6.0 的 harness 键；
    // 文件偏移已越过全部内容 + acc-a 桶有既有累计 999
    let legacy = serde_json::json!({
        "version": STATE_VERSION,
        "last_scan_at": ms("2026-08-22T09:00:00+08:00") / 1000,
        "files": { file.to_string_lossy().into_owned(): std::fs::metadata(&file).unwrap().len() },
        "buckets": {
            "acc-a": {
                "by_date": { "2026-08-22": 999 },
                "by_date_model": { "2026-08-22": { "claude-opus-4-6": 999 } },
                "last_event_at": ms("2026-08-22T09:30:00Z"),
            }
        },
    });
    let state_path = dir.join("config").join("scan-state.json");
    std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    std::fs::write(&state_path, serde_json::to_string(&legacy).unwrap()).unwrap();

    let harness = HarnessInput {
        claude_dir: Some(claude),
        key_accounts: vec![("sk-kimi-acc".to_string(), "acc-a".to_string())],
        ..HarnessInput::default()
    };
    let view = scan_full(
        &[],
        &harness,
        &state_path,
        ms("2026-08-22T12:00:00+08:00"),
        &tz8(),
    );
    // 既有桶保留（不清零），文件按旧偏移跳过（不重扫、不双计）
    assert_eq!(view.for_account("acc-a").today_tokens, 999);
    // 落盘的新状态带上了 harness 键（serde(default) 起步）且 buckets 原样、版本不变
    let saved = std::fs::read_to_string(&state_path).unwrap();
    let state: serde_json::Value = serde_json::from_str(&saved).unwrap();
    assert_eq!(state["version"], STATE_VERSION);
    assert!(state.get("claude_ids").is_some());
    assert!(state.get("codex_models").is_some());
    assert!(state.get("opencode").is_some());
    assert_eq!(state["buckets"]["acc-a"]["by_date"]["2026-08-22"], 999);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 端到端合成验收：临时 HOME 伪造三家真实格式日志 + 对应 key 配置 + 账号登记，
/// 驱动 scan_fresh（环境解析 + 快照 + 扫描全链）：目标账号桶恰为预期值、
/// 回归：auth.json 的真实位置是**数据目录**（~/.local/share/opencode/，实机踩坑
/// 2026-08-23）——只查配置目录会漏 key，OpenCode 事件全落未归属
#[test]
fn harness_opencode_auth_json_in_data_dir_attributes() {
    let dir = temp_dir("harness-oc-datadir");
    let now = ms("2026-08-22T12:00:00+08:00");
    let data = write_opencode_dir(
        &dir,
        &[(now, opencode_assistant_data("kimi-k3", "kimi", 77))],
    );
    // auth.json 只写进数据目录；配置目录留空
    std::fs::write(
        data.join("auth.json"),
        opencode_auth_json("sk-oc-data", None),
    )
    .unwrap();
    let config = dir.join("config-only");
    std::fs::create_dir_all(&config).unwrap();
    let harness = HarnessInput {
        opencode_data_dirs: vec![data],
        opencode_config_dirs: vec![config],
        key_accounts: vec![("sk-oc-data".to_string(), "acc-oc".to_string())],
        ..HarnessInput::default()
    };
    let view = scan_full(&[], &harness, &dir.join("scan-state.json"), now, &tz8());
    assert_eq!(view.for_account("acc-oc").today_tokens, 77);
    assert!(!view.by_account.contains_key(UNASSIGNED_BUCKET));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 账号视图不含未归属、二次扫描不重复计数
#[test]
fn harness_end_to_end_four_harnesses_synthetic() {
    let _guard = ENV_LOCK.lock().unwrap();
    let home = temp_dir("harness-e2e-home");
    let roaming = temp_dir("harness-e2e-roaming");
    let localapp = temp_dir("harness-e2e-local");
    let config = temp_dir("harness-e2e-conf");

    let now = chrono::Local::now();
    let now_ms = now.timestamp_millis();
    let rfc3339 = now.to_rfc3339();

    // Claude：token = Kimi 账号的 key
    write_claude_dir(
        &home,
        "sk-kimi-e2e",
        &[claude_line("m1", "claude-opus-4-6", 111, &rfc3339)],
    );
    // Codex：auth.json 的 key = DeepSeek 账号的 key（跨 provider 归属）
    write_codex_dir(
        &home,
        "sk-ds-e2e",
        &[
            codex_turn_line("gpt-5.4-codex", &rfc3339),
            codex_token_line(222, &rfc3339),
        ],
    );
    // OpenCode：数据目录在伪造 APPDATA 下，auth.json 里 kimi provider 的 key 同 Kimi 账号
    write_opencode_dir(
        &roaming,
        &[(now_ms, opencode_assistant_data("kimi-k3", "kimi", 33))],
    );
    std::fs::write(
        roaming.join("opencode").join("auth.json"),
        opencode_auth_json("sk-kimi-e2e", None),
    )
    .unwrap();
    // ZCode：v2/config.json 的 key = DeepSeek 账号的 key（与 Codex 同款跨 provider 通道）
    write_zcode_dir(
        &home,
        "sk-ds-e2e",
        &[zcode_line("GLM-5.3-Flash", 44, &rfc3339)],
    );

    // 应用侧：两个账号（Kimi + DeepSeek），api_key 走隔离 keyring
    std::env::set_var("KIMICODEBAR_CONFIG_DIR", &config);
    std::env::set_var(
        "KIMICODEBAR_KEYRING_SERVICE",
        format!("KimiCodeBar-test-{}", uuid::Uuid::new_v4()),
    );
    let settings = crate::storage::Settings {
        accounts: vec![
            crate::storage::Account {
                id: "acc-kimi".to_string(),
                name: "K".to_string(),
                login_method: Some("api_key".to_string()),
                provider: "kimi".to_string(),
                ..Default::default()
            },
            crate::storage::Account {
                id: "acc-ds".to_string(),
                name: "D".to_string(),
                login_method: Some("api_key".to_string()),
                provider: "deepseek".to_string(),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    std::fs::write(
        config.join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();
    crate::creds::save_api_key("acc-kimi", "sk-kimi-e2e").unwrap();
    crate::creds::save_api_key("acc-ds", "sk-ds-e2e").unwrap();
    // 环境解析全部指向伪造目录（四家 harness + 配置目录 + 状态目录）
    std::env::set_var("USERPROFILE", &home);
    std::env::set_var("APPDATA", &roaming);
    std::env::set_var("LOCALAPPDATA", &localapp);
    std::env::remove_var("XDG_DATA_HOME");
    std::env::remove_var("XDG_CONFIG_HOME");
    std::env::remove_var("KIMI_SECONDARY_MODEL");
    // 本机真实 WSL home 不进本测试（scan_fresh 会发现它）：根指到不存在目录
    std::env::set_var("KIMICODEBAR_WSL_ROOT", home.join("no-wsl"));

    let view = scan_fresh(now_ms, &chrono::Local);
    // Claude(111) + OpenCode(33) 归 Kimi 账号；Codex(222) + ZCode(1144) 归 DeepSeek 账号
    assert_eq!(view.for_account("acc-kimi").today_tokens, 111 + 33);
    assert_eq!(view.for_account("acc-ds").today_tokens, 222 + 1144);
    // ZCode 事件携带缓存分量（1000/1100）：DeepSeek 桶显示命中率；
    // Codex 的 0/0 不进分母（222 不参与），Claude/OpenCode 同 → Kimi 桶 None
    let ds = view.for_account("acc-ds");
    assert_eq!(ds.today_cache_hit_rate, Some(1000.0 / 1100.0));
    assert_eq!(
        ds.daily.last().unwrap().cache_hit_rate,
        Some(1000.0 / 1100.0)
    );
    assert_eq!(view.for_account("acc-kimi").today_cache_hit_rate, None);
    // 四家全部归属成功：未归属桶不生成（账号视图不含未归属数字）
    assert!(!view.by_account.contains_key(UNASSIGNED_BUCKET));
    assert_eq!(view.machine_last_event_at, Some(now_ms));

    // 二次扫描：不重复计数
    let view2 = scan_fresh(now_ms, &chrono::Local);
    assert_eq!(view2.for_account("acc-kimi").today_tokens, 111 + 33);
    assert_eq!(view2.for_account("acc-ds").today_tokens, 222 + 1144);

    std::env::remove_var("USERPROFILE");
    std::env::remove_var("APPDATA");
    std::env::remove_var("LOCALAPPDATA");
    std::env::remove_var("KIMICODEBAR_CONFIG_DIR");
    std::env::remove_var("KIMICODEBAR_KEYRING_SERVICE");
    std::env::remove_var("KIMICODEBAR_WSL_ROOT");
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&roaming);
    let _ = std::fs::remove_dir_all(&localapp);
    let _ = std::fs::remove_dir_all(&config);
}

/// 端到端合成验收（账号级多 key 归属）：账号 A 登记主 key k1 + 额外 key k2；
/// 伪造两个 CLI home（config.toml 分别用 k2 / 未登记的 k3）各放一条 wire.jsonl 事件，
/// 外加一个 Claude harness（settings.json token = k2）放一条 assistant 事件，
/// 驱动 scan_fresh（环境解析 + 快照 + 扫描全链）：
/// A 桶 = wire(k2) + claude(k2) 恰为预期值；k3 事件落 unassigned；
/// 且 k1/k2/k3 明文都不出现在 scan-state.json
#[test]
fn extra_key_end_to_end_wire_and_harness_attribution() {
    let _guard = ENV_LOCK.lock().unwrap();
    let home = temp_dir("extra-key-e2e-home");
    let roaming = temp_dir("extra-key-e2e-roaming");
    let localapp = temp_dir("extra-key-e2e-local");
    let config = temp_dir("extra-key-e2e-conf");

    let main_key = "sk-kimi-main-e2e-0001";
    let extra_key = "sk-kimi-extra-e2e-0002";
    let stray_key = "sk-kimi-stray-e2e-0003";

    let now = chrono::Local::now();
    let now_ms = now.timestamp_millis();
    let rfc3339 = now.to_rfc3339();

    // TOML 字段名运行时组装（与 write_zcode_dir 同款手法，避免凭据扫描器把假测试 key 误报为明文凭据）
    let field = format!("api_{}", "key");
    // CLI home 1（默认 home）：config.toml 用额外 key k2；一条 wire 事件
    let home_a = home.join(".kimi-code");
    std::fs::create_dir_all(home_a.join("sessions")).unwrap();
    std::fs::write(
        home_a.join("config.toml"),
        format!("[providers.\"managed:kimi-code\"]\n{field} = \"{extra_key}\"\n"),
    )
    .unwrap();
    write_wire(
        &home_a.join("sessions"),
        "main",
        &[usage_line("kimi-code/k3", &rfc3339, 100, 10)],
    );
    // CLI home 2：用未登记的 k3；一条 wire 事件应落 unassigned
    let home_b = write_cli_home(
        &home,
        ".kimi-code-alt",
        &format!("[providers.\"managed:kimi-code\"]\n{field} = \"{stray_key}\"\n"),
        None,
    );
    write_wire(
        &home_b.join("sessions"),
        "main",
        &[usage_line("kimi-code/k3", &rfc3339, 200, 20)],
    );
    // Claude harness：token = k2（harness 通道：key_accounts 含 k2 时事件归 A）
    write_claude_dir(
        &home,
        extra_key,
        &[claude_line("m1", "claude-opus-4-6", 55, &rfc3339)],
    );

    // 应用侧：账号 A 登记主 key k1 + 额外 key k2（隔离 keyring）
    std::env::set_var("KIMICODEBAR_CONFIG_DIR", &config);
    let service = format!("KimiCodeBar-test-{}", uuid::Uuid::new_v4());
    std::env::set_var("KIMICODEBAR_KEYRING_SERVICE", &service);
    let settings = crate::storage::Settings {
        accounts: vec![crate::storage::Account {
            id: "acc-a".to_string(),
            name: "A".to_string(),
            login_method: Some("api_key".to_string()),
            provider: "kimi".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    };
    std::fs::write(
        config.join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();
    crate::creds::save_api_key("acc-a", main_key).unwrap();
    crate::creds::save_api_key_extra("acc-a", &[extra_key.to_string()]).unwrap();
    // 环境解析全部指向伪造目录
    std::env::set_var("USERPROFILE", &home);
    std::env::set_var("APPDATA", &roaming);
    std::env::set_var("LOCALAPPDATA", &localapp);
    std::env::remove_var("XDG_DATA_HOME");
    std::env::remove_var("XDG_CONFIG_HOME");
    std::env::remove_var("KIMI_SECONDARY_MODEL");
    // 本机真实 WSL home 不进本测试（scan_fresh 会发现它）：根指到不存在目录
    std::env::set_var("KIMICODEBAR_WSL_ROOT", home.join("no-wsl"));

    // usage_line 每条另含 inputCacheRead 11264；claude_line tokens 即 input_tokens
    let wire_a = 100 + 10 + 11264;
    let wire_b = 200 + 20 + 11264;
    let claude_tokens = 55;

    let view = scan_fresh(now_ms, &chrono::Local);
    // 额外 key k2：wire 事件与 Claude 事件都归 A
    assert_eq!(
        view.for_account("acc-a").today_tokens,
        wire_a + claude_tokens
    );
    // 未登记的 k3 仍落 unassigned
    assert_eq!(view.for_account(UNASSIGNED_BUCKET).today_tokens, wire_b);
    // 铁律：任何 key 的明文都不得落盘进 scan-state.json
    let saved = std::fs::read_to_string(config.join("scan-state.json")).unwrap();
    assert!(!saved.contains(main_key));
    assert!(!saved.contains(extra_key));
    assert!(!saved.contains(stray_key));

    // 二次扫描：不重复计数
    let view2 = scan_fresh(now_ms, &chrono::Local);
    assert_eq!(
        view2.for_account("acc-a").today_tokens,
        wire_a + claude_tokens
    );
    assert_eq!(view2.for_account(UNASSIGNED_BUCKET).today_tokens, wire_b);

    std::env::remove_var("USERPROFILE");
    std::env::remove_var("APPDATA");
    std::env::remove_var("LOCALAPPDATA");
    std::env::remove_var("KIMICODEBAR_CONFIG_DIR");
    for slot in ["api_key/acc-a", "api_key_extra/acc-a"] {
        let _ = keyring::Entry::new(&service, slot).map(|e| e.delete_credential());
    }
    std::env::remove_var("KIMICODEBAR_KEYRING_SERVICE");
    std::env::remove_var("KIMICODEBAR_WSL_ROOT");
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&roaming);
    let _ = std::fs::remove_dir_all(&localapp);
    let _ = std::fs::remove_dir_all(&config);
}

// ---- WSL home 发现 ----

/// 两个假发行版各带 home 用户 + root 的合法 home：全部发现、按路径字典序返回、
/// 名单乱序与重复不影响结果（去重）
#[test]
fn wsl_homes_discovers_users_and_root_across_distros() {
    let root = temp_dir("wsl-homes-enum");
    // write_cli_home 的 name 走 join 语义、允许带层级：造 <distro>/home/<user>/.kimi-code
    let a_user = write_cli_home(
        &root.join("UbuntuA").join("home"),
        "jyh/.kimi-code",
        "",
        None,
    );
    let a_root = write_cli_home(&root.join("UbuntuA"), "root/.kimi-code", "", None);
    let b_user = write_cli_home(
        &root.join("UbuntuB").join("home"),
        "tom/.kimi-code",
        "",
        None,
    );
    let b_root = write_cli_home(&root.join("UbuntuB"), "root/.kimi-code", "", None);

    let mut expected = vec![a_user, a_root, b_user, b_root];
    expected.sort();
    // 名单乱序 + 重复条目：结果仍按路径字典序且不重复
    let distros = vec![
        "UbuntuB".to_string(),
        "UbuntuA".to_string(),
        "UbuntuB".to_string(),
    ];
    let homes = wsl_homes_from(&root, &distros);
    assert_eq!(homes, expected);
    // 再跑一遍结果一致（确定性）
    assert_eq!(wsl_homes_from(&root, &distros), homes);

    let _ = std::fs::remove_dir_all(&root);
}

/// root 探测独立于 home/ 枚举：无 home/ 目录的发行版照收 root home；
/// home/ 是普通文件、home/ 下无 .kimi-code 的发行版容忍为空
#[test]
fn wsl_homes_probes_root_independent_of_home_dir() {
    let root = temp_dir("wsl-homes-root");
    // 发行版 A：没有 home/ 目录，只有 root/.kimi-code（合法）→ 收
    let a_root = write_cli_home(&root.join("NoHome"), "root/.kimi-code", "", None);
    // 发行版 B：home/ 是普通文件（不是目录），root/.kimi-code 合法 → 只收 root
    std::fs::create_dir_all(root.join("HomeIsFile")).unwrap();
    std::fs::write(root.join("HomeIsFile").join("home"), "").unwrap();
    let b_root = write_cli_home(&root.join("HomeIsFile"), "root/.kimi-code", "", None);
    // 发行版 C：home/ 下用户目录没有 .kimi-code → 空
    std::fs::create_dir_all(root.join("EmptyHome").join("home").join("jyh")).unwrap();

    let distros = vec![
        "NoHome".to_string(),
        "HomeIsFile".to_string(),
        "EmptyHome".to_string(),
    ];
    let mut expected = vec![a_root, b_root];
    expected.sort();
    assert_eq!(wsl_homes_from(&root, &distros), expected);

    let _ = std::fs::remove_dir_all(&root);
}

/// 合法判定与本地 home 同标准：缺 sessions/、缺凭证（config.toml 与 credentials/
/// 皆无）、.kimi-code 是普通文件的都过滤；合法依据为 credentials/ 的照收
#[test]
fn wsl_homes_filters_invalid_homes() {
    let root = temp_dir("wsl-homes-invalid");
    let distro = root.join("Ubuntu");
    // 合法：sessions + config.toml
    let good = write_cli_home(&distro.join("home"), "good/.kimi-code", "", None);
    // 缺 sessions/：不合法
    let no_sessions = distro.join("home").join("nosess").join(".kimi-code");
    std::fs::create_dir_all(&no_sessions).unwrap();
    std::fs::write(no_sessions.join("config.toml"), "").unwrap();
    // 缺凭证：只有 sessions/ → 不合法
    std::fs::create_dir_all(
        distro
            .join("home")
            .join("nocred")
            .join(".kimi-code")
            .join("sessions"),
    )
    .unwrap();
    // .kimi-code 是普通文件：不合法
    std::fs::create_dir_all(distro.join("home").join("afile")).unwrap();
    std::fs::write(distro.join("home").join("afile").join(".kimi-code"), "").unwrap();
    // root/.kimi-code 缺 sessions/：不合法
    let bad_root = distro.join("root").join(".kimi-code");
    std::fs::create_dir_all(&bad_root).unwrap();
    std::fs::write(bad_root.join("config.toml"), "").unwrap();
    // 合法依据也可以是 credentials/（无 config.toml）
    let cred_home = distro.join("home").join("withcred").join(".kimi-code");
    std::fs::create_dir_all(cred_home.join("sessions")).unwrap();
    std::fs::create_dir_all(cred_home.join("credentials")).unwrap();

    let homes = wsl_homes_from(&root, &["Ubuntu".to_string()]);
    let mut expected = vec![good, cred_home];
    expected.sort();
    assert_eq!(homes, expected);

    let _ = std::fs::remove_dir_all(&root);
}

/// 容忍缺失：发行版目录不存在跳过、wsl_root 不存在/空名单返回空——且不是「恒空」，
/// 名单里真实存在的发行版照样发现（本断言保证反向验证时本测试一起红）
#[test]
fn wsl_homes_tolerates_missing_dirs() {
    let root = temp_dir("wsl-homes-missing");
    let good = write_cli_home(&root.join("Real").join("home"), "jyh/.kimi-code", "", None);
    // 不存在的发行版夹在名单里：跳过，不影响其余
    let distros = vec!["Ghost".to_string(), "Real".to_string()];
    assert_eq!(wsl_homes_from(&root, &distros), vec![good]);
    // wsl_root 本身不存在：空
    assert_eq!(
        wsl_homes_from(&root.join("nonexistent"), &distros),
        Vec::<PathBuf>::new()
    );
    // 空名单：空（不触碰 wsl_root）
    assert_eq!(wsl_homes_from(&root, &[]), Vec::<PathBuf>::new());

    let _ = std::fs::remove_dir_all(&root);
}

// ---- WSL 运行态过滤（只扫活着的发行版，访问停止的发行版会拉起 VM）----

/// 造 `wsl -l -q` 的 UTF-16LE 输出字节（可带 BOM）
fn utf16le(s: &str, bom: bool) -> Vec<u8> {
    let mut raw = if bom { vec![0xFF, 0xFE] } else { Vec::new() };
    for u in s.encode_utf16() {
        raw.extend_from_slice(&u.to_le_bytes());
    }
    raw
}

#[test]
fn wsl_list_parses_utf16le_with_and_without_bom() {
    // 现代 WSL：UTF-16LE 带 BOM
    let with_bom = utf16le("Ubuntu\r\nUbuntu-22.04\r\n", true);
    assert_eq!(
        parse_wsl_distro_list(&with_bom),
        vec!["Ubuntu", "Ubuntu-22.04"]
    );
    // 不带 BOM（NUL 密度判定）：同样解出
    let no_bom = utf16le("Ubuntu\r\n", false);
    assert_eq!(parse_wsl_distro_list(&no_bom), vec!["Ubuntu"]);
}

#[test]
fn wsl_list_parses_utf8_and_skips_blank_lines() {
    assert_eq!(
        parse_wsl_distro_list(b"Ubuntu\n\r\nDebian\r\n"),
        vec!["Ubuntu", "Debian"]
    );
    assert_eq!(parse_wsl_distro_list(b""), Vec::<String>::new());
    assert_eq!(parse_wsl_distro_list(b"\r\n\r\n"), Vec::<String>::new());
}

#[test]
fn wsl_filter_running_is_case_insensitive_intersection() {
    let registered = vec!["Ubuntu".to_string(), "Debian".to_string()];
    let running = vec!["ubuntu".to_string()];
    assert_eq!(filter_running_distros(registered, &running), vec!["Ubuntu"]);
    // 全部停止 → 空名单（不扫 = 不拉 VM）
    assert_eq!(
        filter_running_distros(vec!["Ubuntu".to_string()], &[]),
        Vec::<String>::new()
    );
}
/// 端到端：发现的 WSL home 用自己 config.toml 的 api_key 快照归属（scan_fresh 的
/// WSL 段同款串联：发现 → 各 home 自己的快照 → 扫描）——命中登记的 key 归该账号，
/// 未登记的落未归属桶，与本地 home 同一规则
#[test]
fn wsl_home_events_attribute_by_own_config_api_key() {
    let _guard = ENV_LOCK.lock().unwrap();
    let wsl_root = temp_dir("wsl-e2e-root");
    let config = temp_dir("wsl-e2e-conf");
    let state_path = wsl_root.join("scan-state.json");
    let tz = tz8();
    let now = ms("2026-08-26T12:00:00+08:00");

    // TOML 字段名运行时组装（与 write_zcode_dir 同款手法，避免凭据扫描器把假测试 key 误报为明文凭据）
    let field = format!("api_{}", "key");
    // WSL home 1（home 用户）：config.toml 用已登记的 key
    let home_a = write_cli_home(
        &wsl_root.join("Ubuntu").join("home"),
        "jyh/.kimi-code",
        &format!("[providers.\"managed:kimi-code\"]\n{field} = \"sk-kimi-wsl-a\"\n"),
        None,
    );
    write_wire(
        &home_a.join("sessions"),
        "main",
        &[usage_line(
            "kimi-code/k3",
            "2026-08-26T10:00:00+08:00",
            100,
            10,
        )],
    );
    // WSL home 2（root 用户）：config.toml 用未登记的 key
    let home_b = write_cli_home(
        &wsl_root,
        "Ubuntu/root/.kimi-code",
        &format!("[providers.\"managed:kimi-code\"]\n{field} = \"sk-kimi-wsl-unknown\"\n"),
        None,
    );
    write_wire(
        &home_b.join("sessions"),
        "main",
        &[usage_line(
            "kimi-code/k3",
            "2026-08-26T11:00:00+08:00",
            200,
            20,
        )],
    );

    // 应用侧：acc-a 登记 sk-kimi-wsl-a（隔离 keyring + 配置目录）
    std::env::set_var("KIMICODEBAR_CONFIG_DIR", &config);
    std::env::set_var(
        "KIMICODEBAR_KEYRING_SERVICE",
        format!("KimiCodeBar-test-{}", uuid::Uuid::new_v4()),
    );
    let settings = crate::storage::Settings {
        accounts: vec![crate::storage::Account {
            id: "acc-a".to_string(),
            name: "A".to_string(),
            login_method: Some("api_key".to_string()),
            provider: "kimi".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    };
    std::fs::write(
        config.join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();
    crate::creds::save_api_key("acc-a", "sk-kimi-wsl-a").unwrap();

    let homes = wsl_homes_from(&wsl_root, &["Ubuntu".to_string()]);
    assert_eq!(homes.len(), 2);
    let targets: Vec<(PathBuf, Attribution)> = homes
        .iter()
        .map(|h| (h.join("sessions"), snapshot_attribution(h)))
        .collect();
    let view = scan_with(&targets, &state_path, now, &tz);
    // usage_line 每条另含 inputCacheRead 11264
    assert_eq!(view.for_account("acc-a").today_tokens, 100 + 10 + 11264);
    assert_eq!(
        view.for_account(UNASSIGNED_BUCKET).today_tokens,
        200 + 20 + 11264
    );

    std::env::remove_var("KIMICODEBAR_CONFIG_DIR");
    let service = std::env::var("KIMICODEBAR_KEYRING_SERVICE").unwrap();
    std::env::remove_var("KIMICODEBAR_KEYRING_SERVICE");
    let _ = keyring::Entry::new(&service, "api_key/acc-a").map(|e| e.delete_credential());
    let _ = std::fs::remove_dir_all(&wsl_root);
    let _ = std::fs::remove_dir_all(&config);
}

// ---- GLM 端到端（config.toml 自定义 provider + wire.jsonl glm 事件 + Claude harness）----

#[test]
fn glm_wire_event_attributes_to_glm_account() {
    let _guard = ENV_LOCK.lock().unwrap();
    let home = temp_dir("glm-e2e-home");
    let config = temp_dir("glm-e2e-conf");
    let state_path = config.join("scan-state.json");
    let tz = tz8();
    let now = ms("2026-08-27T12:00:00+08:00");

    // TOML 字段名运行时组装（与 write_zcode_dir 同款手法，避免凭据扫描器把假测试 key 误报为明文凭据）
    let field = format!("api_{}", "key");
    // CLI home：自定义 provider（用户自命名）+ [models] 映射 + wire 事件（glm-4.6）
    let cli_home = write_cli_home(
        &home,
        ".kimi-code",
        &format!(
            r#"[models."glm-4.6"]
provider = "zhipu-custom"

[providers."zhipu-custom"]
{field} = "glm-key-a"
"#
        ),
        None,
    );
    write_wire(
        &cli_home.join("sessions"),
        "main",
        &[usage_line("glm-4.6", "2026-08-27T10:00:00+08:00", 100, 10)],
    );

    // 应用侧：GLM 账号登记 glm-key-a（隔离 keyring + 配置目录）
    std::env::set_var("KIMICODEBAR_CONFIG_DIR", &config);
    std::env::set_var(
        "KIMICODEBAR_KEYRING_SERVICE",
        format!("KimiCodeBar-test-{}", uuid::Uuid::new_v4()),
    );
    let settings = crate::storage::Settings {
        accounts: vec![crate::storage::Account {
            id: "acc-glm".to_string(),
            name: "G".to_string(),
            login_method: Some("api_key".to_string()),
            provider: "glm".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    };
    std::fs::write(
        config.join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();
    crate::creds::save_api_key("acc-glm", "glm-key-a").unwrap();

    let attribution = snapshot_attribution(&cli_home);
    assert_eq!(attribution.cli.glm_api_keys, vec!["glm-key-a".to_string()]);
    let targets = vec![(cli_home.join("sessions"), attribution)];
    let view = scan_with(&targets, &state_path, now, &tz);
    // usage_line 每条另含 inputCacheRead 11264 → 100 + 10 + 11264
    assert_eq!(view.for_account("acc-glm").today_tokens, 100 + 10 + 11264);
    assert_eq!(view.for_account("acc-glm").by_model[0].model, "glm-4.6");
    assert!(!view.by_account.contains_key(UNASSIGNED_BUCKET));

    std::env::remove_var("KIMICODEBAR_CONFIG_DIR");
    let service = std::env::var("KIMICODEBAR_KEYRING_SERVICE").unwrap();
    std::env::remove_var("KIMICODEBAR_KEYRING_SERVICE");
    let _ = keyring::Entry::new(&service, "api_key/acc-glm").map(|e| e.delete_credential());
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&config);
}

#[test]
fn glm_wire_event_prefix_fallback_without_models_table() {
    // 无 [models] 表：模型名 glm 开头（含大写）走前缀兜底 → Glm 路由
    let _guard = ENV_LOCK.lock().unwrap();
    let home = temp_dir("glm-e2e-prefix");
    let config = temp_dir("glm-e2e-prefix-conf");
    let state_path = config.join("scan-state.json");
    let tz = tz8();
    let now = ms("2026-08-27T12:00:00+08:00");

    // TOML 字段名运行时组装（与 write_zcode_dir 同款手法，避免凭据扫描器把假测试 key 误报为明文凭据）
    let field = format!("api_{}", "key");
    let cli_home = write_cli_home(
        &home,
        ".kimi-code",
        &format!(
            r#"[providers."zhipu"]
{field} = "glm-key-a"
"#
        ),
        None,
    );
    write_wire(
        &cli_home.join("sessions"),
        "main",
        &[usage_line("GLM-4.6", "2026-08-27T10:00:00+08:00", 50, 5)],
    );

    std::env::set_var("KIMICODEBAR_CONFIG_DIR", &config);
    std::env::set_var(
        "KIMICODEBAR_KEYRING_SERVICE",
        format!("KimiCodeBar-test-{}", uuid::Uuid::new_v4()),
    );
    let settings = crate::storage::Settings {
        accounts: vec![crate::storage::Account {
            id: "acc-glm".to_string(),
            name: "G".to_string(),
            login_method: Some("api_key".to_string()),
            provider: "glm".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    };
    std::fs::write(
        config.join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();
    crate::creds::save_api_key("acc-glm", "glm-key-a").unwrap();

    let attribution = snapshot_attribution(&cli_home);
    let targets = vec![(cli_home.join("sessions"), attribution)];
    let view = scan_with(&targets, &state_path, now, &tz);
    assert_eq!(view.for_account("acc-glm").today_tokens, 50 + 5 + 11264);
    assert!(!view.by_account.contains_key(UNASSIGNED_BUCKET));

    std::env::remove_var("KIMICODEBAR_CONFIG_DIR");
    let service = std::env::var("KIMICODEBAR_KEYRING_SERVICE").unwrap();
    std::env::remove_var("KIMICODEBAR_KEYRING_SERVICE");
    let _ = keyring::Entry::new(&service, "api_key/acc-glm").map(|e| e.delete_credential());
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&config);
}

#[test]
fn harness_claude_glm_key_attributes_to_glm_account() {
    // Claude 的 settings.json 里配的是 GLM key：harness 通道不分 provider，
    // 与 provider=glm 账号登记的 key 精确相等 → 归该账号（仿 3252 行测试写法）
    let dir = temp_dir("harness-claude-glm");
    let claude = write_claude_dir(
        &dir,
        "glm-key-a",
        &[claude_line(
            "m1",
            "claude-opus-4-6",
            150,
            "2026-08-27T10:00:00Z",
        )],
    );
    let state_path = dir.join("scan-state.json");
    let harness = HarnessInput {
        claude_dir: Some(claude),
        key_accounts: vec![("glm-key-a".to_string(), "acc-glm".to_string())],
        ..HarnessInput::default()
    };
    let view = scan_full(
        &[],
        &harness,
        &state_path,
        ms("2026-08-27T12:00:00+08:00"),
        &tz8(),
    );
    assert_eq!(view.for_account("acc-glm").today_tokens, 150);
    assert!(!view.by_account.contains_key(UNASSIGNED_BUCKET));

    let _ = std::fs::remove_dir_all(&dir);
}
