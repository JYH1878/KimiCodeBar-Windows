//! 本地 Token 消耗统计：增量扫描 Kimi Code 会话的 wire.jsonl 用量事件，
//! 聚合为今日/昨日/最近 7 天/分模型累计（语义移植自 macOS 版 KimiLocalUsage.swift）。
//!
//! 数据源：`{userprofile}/.kimi-code/sessions/**/wire.jsonl`（递归遍历），逐行 JSON，
//! 只认 `{"type":"usage.record",...}` 事件，实测样例：
//! `{"type":"usage.record","model":"kimi-code/k3","usage":{"inputOther":11592,"output":504,"inputCacheRead":11264,"inputCacheCreation":0},"usageScope":"turn","time":1784973672311}`
//! （time 为 epoch 毫秒；tokens = inputOther + output + inputCacheRead + inputCacheCreation；
//! usageScope 实测恒为 "turn"，不作过滤）
//!
//! `__secondary__` 哨兵：开启 Kimi Code 的 secondary_model 实验后，子 agent 的用量事件
//! model 落为字面量 `"__secondary__"`（实测：
//! `{"type":"usage.record","model":"__secondary__","usage":{"inputOther":9322,"output":132,...},...}`）。
//! 出统计视图后把该桶并入真实模型（resolve_secondary_model + fold_secondary_model）：
//! 环境变量 KIMI_SECONDARY_MODEL（非空）优先，其次 `~/.kimi-code/config.toml` 的
//! `[secondary_model].model`；折叠只在展示层做、不落盘——scan-state.json 保留原始哨兵桶，
//! 用户改配 secondary 后下次扫描自动按新映射显示；两处都解析不到时保留原样展示。
//!
//! 增量扫描：`{config_dir}/scan-state.json` 记录每个文件的已读字节偏移与累计聚合，
//! 每次只读各文件偏移之后的新字节；文件被截断/重写（长度 < 偏移）回退为从头读。
//! 状态全量原子写（临时文件 + rename，与 storage.rs 同款）。
//! 扫描节流：进程内缓存结果，距上次扫描 < 180 秒直接返回缓存。
//!
//! 与 macOS 原版的已知差异（原版仓库不在本机，按钉死的契约语义实现）：
//! - daily 固定输出最近 7 个自然日（无消耗的日子补 0），保证前端折线图逐日连续；
//! - 按日×模型的累计聚合随偏移一起持久化在 scan-state.json：
//!   增量读取下"今日分模型 by_model"必须靠落盘的按日×模型累计值，否则每次都得全量重读；
//!   by_model 语义为今日（与卡片主体"今日/近 7 天"一致），不是全部时间累计；
//! - 旧版状态（只有按日总和、没有按日×模型）在下次扫描时检测并全量重扫一次完成迁移；
//! - 文件截断回退为整文件重读，该文件的旧贡献理论上可能重复计数一次
//!   （会话文件按 uuid 命名、只增不改，实际不会触发）；
//! - 已消失文件的偏移会被清理，同名新文件从头读，不会按旧偏移跳过开头。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, NaiveDate, TimeZone};
use serde::{Deserialize, Serialize};

use crate::history::HistoryPoint;

/// 扫描节流：距上次扫描小于该秒数直接返回进程内缓存结果
const THROTTLE_SECS: i64 = 180;
/// 最近 N 个自然日逐日消耗
const DAILY_DAYS: i64 = 7;
/// 按日聚合在状态文件里的保留窗口（天）；展示只用最近 7 天，多留冗余
const BY_DATE_RETENTION_DAYS: i64 = 30;
/// 分模型展示上限（今日，tokens 降序）
const TOP_MODELS: usize = 5;
/// 副模型哨兵：secondary_model 实验下子 agent 的 usage.record model 落该字面值
const SECONDARY_SENTINEL: &str = "__secondary__";
/// CSV 表头（与导出约定一致）：时间为本地 ISO（YYYY-MM-DDTHH:mm:ss）
const CSV_HEADER: &str = "time,weekly,five_hour,monthly";

/// 某一天的消耗（与 src/types.ts 的 DailyUsage 一一对应）
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DailyUsage {
    /// 本地日期 YYYY-MM-DD
    pub date: String,
    pub tokens: u64,
}

/// 某模型的累计消耗（与 src/types.ts 的 ModelUsage 一一对应）
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ModelUsage {
    pub model: String,
    pub tokens: u64,
}

/// 本地 token 消耗统计（get_local_usage 的返回，与 types.ts LocalUsageStats 一一对应）
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct LocalUsageStats {
    /// 今日总消耗
    pub today_tokens: u64,
    /// 昨日总消耗
    pub yesterday_tokens: u64,
    /// 最近 7 天逐日消耗（升序，无消耗的日子补 0）
    pub daily: Vec<DailyUsage>,
    /// 按模型累计（今日，tokens 降序 top 5）
    pub by_model: Vec<ModelUsage>,
    /// 上次扫描时间（epoch 秒），未扫过为 null
    pub last_scan_at: Option<i64>,
}

/// 进程内结果缓存（节流用）：上次扫描完成时刻（epoch 秒）+ 结果
static SCAN_CACHE: Mutex<Option<(i64, LocalUsageStats)>> = Mutex::new(None);

/// 扫描一次本地用量：距上次 < 180 秒返回进程内缓存，否则增量扫描并落盘状态。
/// 永不失败：sessions 目录不存在、单文件读失败、状态写失败均容忍为（部分）空结果
/// —— 与 history 一致，统计是派生数据，丢了重扫即可。
pub fn scan() -> LocalUsageStats {
    let now = chrono::Local::now();
    let now_secs = now.timestamp();
    {
        let cache = SCAN_CACHE.lock().unwrap();
        if let Some((scanned_at, stats)) = &*cache {
            if now_secs - *scanned_at < THROTTLE_SECS {
                return stats.clone();
            }
        }
    }
    let mut stats = scan_with(
        &sessions_dir().unwrap_or_default(),
        &state_file_path(),
        now.timestamp_millis(),
        &chrono::Local,
    );
    // __secondary__ 桶并入真实副模型（展示层折叠，不落盘；解析不到保留原样）
    if let Some(target) = resolve_secondary_model() {
        fold_secondary_model(&mut stats.by_model, &target);
    }
    *SCAN_CACHE.lock().unwrap() = Some((now_secs, stats.clone()));
    stats
}

/// 导出用量报告：history.json 采样点写为 CSV 到 `{config_dir}/exports/usage-YYYYMMDD-HHmmss.csv`，
/// 并把 history.json 原文复制到同目录；返回 exports 目录路径（reveal 由命令层负责）
pub fn export_usage_report() -> Result<PathBuf, String> {
    let config_dir = crate::storage::config_dir();
    let points = crate::history::HistoryStore::load().into_points();
    export_report_to(
        &config_dir.join("exports"),
        &config_dir.join("history.json"),
        &points,
        chrono::Local::now(),
    )
}

// ---------------------------------------------------------------------------
// 以下为内部实现
// ---------------------------------------------------------------------------

/// 一条 usage.record 事件的解析结果
#[derive(Debug, PartialEq)]
struct UsageEvent {
    /// epoch 毫秒
    ts_ms: i64,
    model: String,
    tokens: u64,
}

/// 解析单行 wire.jsonl：合法 usage.record 返回事件；其他类型 / 坏 JSON / 缺 time 返回 None。
/// 缺 model 计入 "unknown" 桶（token 是真实烧掉的，不该因缺标签丢弃）；
/// usage 字段缺失按 0 计。纯函数，可直接单测。
fn parse_usage_line(line: &str) -> Option<UsageEvent> {
    #[derive(Deserialize)]
    struct WireLine {
        #[serde(rename = "type")]
        kind: String,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        usage: Option<UsageFields>,
        #[serde(default)]
        time: Option<i64>,
    }

    #[derive(Default, Deserialize)]
    struct UsageFields {
        #[serde(rename = "inputOther", default)]
        input_other: u64,
        #[serde(default)]
        output: u64,
        #[serde(rename = "inputCacheRead", default)]
        input_cache_read: u64,
        #[serde(rename = "inputCacheCreation", default)]
        input_cache_creation: u64,
    }

    let line: WireLine = serde_json::from_str(line).ok()?;
    if line.kind != "usage.record" {
        return None;
    }
    // 无法定位日期的事件没有统计价值（真实数据 time 恒存在）
    let ts_ms = line.time?;
    let usage = line.usage.unwrap_or_default();
    Some(UsageEvent {
        ts_ms,
        model: line.model.unwrap_or_else(|| "unknown".to_string()),
        tokens: usage.input_other
            + usage.output
            + usage.input_cache_read
            + usage.input_cache_creation,
    })
}

/// epoch 毫秒 → 指定时区的本地日期键 YYYY-MM-DD；时间戳溢出为 None
fn date_key<Tz: TimeZone>(ts_ms: i64, tz: &Tz) -> Option<String>
where
    Tz::Offset: std::fmt::Display,
{
    let dt = tz.timestamp_millis_opt(ts_ms).single()?;
    Some(dt.format("%Y-%m-%d").to_string())
}

/// 按日累计聚合器：扫描产出的事件逐条喂入，最后按"今天"出统计视图。
/// 聚合结果随扫描状态一起落盘（增量读取下今日分模型 by_model 的前提）。
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct UsageAggregator {
    /// 本地日期（YYYY-MM-DD）→ 累计 tokens
    #[serde(default)]
    by_date: HashMap<String, u64>,
    /// 本地日期（YYYY-MM-DD）→ 模型 → 该日累计 tokens（今日分模型占比的来源）
    #[serde(default)]
    by_date_model: HashMap<String, HashMap<String, u64>>,
}

impl UsageAggregator {
    /// 喂入一条事件：按本地日期与按日×模型分别累计。
    /// 日期键取不出（时间戳溢出）时丢弃该事件（与解析层缺 time 同策略）
    fn add<Tz: TimeZone>(&mut self, event: &UsageEvent, tz: &Tz)
    where
        Tz::Offset: std::fmt::Display,
    {
        if let Some(date) = date_key(event.ts_ms, tz) {
            *self.by_date.entry(date.clone()).or_insert(0) += event.tokens;
            let models = self.by_date_model.entry(date).or_default();
            *models.entry(event.model.clone()).or_insert(0) += event.tokens;
        }
    }

    /// 丢弃 30 天前的按日聚合（两个映射同一窗口），控制状态文件体积
    fn prune(&mut self, today: NaiveDate) {
        let cutoff = (today - chrono::Duration::days(BY_DATE_RETENTION_DAYS))
            .format("%Y-%m-%d")
            .to_string();
        // 日期键零填充定长，字符串序即日期序
        self.by_date.retain(|date, _| date >= &cutoff);
        self.by_date_model.retain(|date, _| date >= &cutoff);
    }

    /// 由累计聚合出统计视图：today 为本地今天；
    /// daily 为最近 7 个自然日（升序、缺日补 0）；by_model 为今日分模型降序 top 5
    fn finish(&self, today: NaiveDate, last_scan_at: Option<i64>) -> LocalUsageStats {
        let day_tokens = |date: NaiveDate| {
            self.by_date
                .get(&date.format("%Y-%m-%d").to_string())
                .copied()
                .unwrap_or(0)
        };
        let daily = (0..DAILY_DAYS)
            .rev()
            .map(|i| {
                let date = today - chrono::Duration::days(i);
                DailyUsage {
                    date: date.format("%Y-%m-%d").to_string(),
                    tokens: day_tokens(date),
                }
            })
            .collect();
        let mut by_model: Vec<ModelUsage> = self
            .by_date_model
            .get(&today.format("%Y-%m-%d").to_string())
            .map(|models| {
                models
                    .iter()
                    .map(|(model, tokens)| ModelUsage {
                        model: model.clone(),
                        tokens: *tokens,
                    })
                    .collect()
            })
            .unwrap_or_default();
        // tokens 降序；并列按模型名升序，保证输出确定
        by_model.sort_by(|a, b| b.tokens.cmp(&a.tokens).then_with(|| a.model.cmp(&b.model)));
        by_model.truncate(TOP_MODELS);
        LocalUsageStats {
            today_tokens: day_tokens(today),
            yesterday_tokens: day_tokens(today - chrono::Duration::days(1)),
            daily,
            by_model,
            last_scan_at,
        }
    }
}

/// 把 by_model 里的 __secondary__ 桶并入 target 桶（tokens 相加；target 不在榜则顶替进来）。
/// 合并后按 finish 同款规则重排（tokens 降序、并列按名升序）并重截 top 5
fn fold_secondary_model(by_model: &mut Vec<ModelUsage>, target: &str) {
    let Some(idx) = by_model.iter().position(|m| m.model == SECONDARY_SENTINEL) else {
        return;
    };
    let secondary = by_model.remove(idx);
    match by_model.iter_mut().find(|m| m.model == target) {
        Some(m) => m.tokens += secondary.tokens,
        None => by_model.push(ModelUsage {
            model: target.to_string(),
            tokens: secondary.tokens,
        }),
    }
    by_model.sort_by(|a, b| b.tokens.cmp(&a.tokens).then_with(|| a.model.cmp(&b.model)));
    by_model.truncate(TOP_MODELS);
}

/// 扫描状态（scan-state.json）：文件偏移 + 累计聚合。损坏/不存在容忍为空状态重新全扫
#[derive(Debug, Default, Serialize, Deserialize)]
struct ScanState {
    /// 上次完成扫描时间（epoch 秒）
    #[serde(default)]
    last_scan_at: Option<i64>,
    /// 文件路径 → 已读字节偏移
    #[serde(default)]
    files: HashMap<String, u64>,
    /// 累计聚合（增量读取下全时间统计的来源）
    #[serde(default)]
    totals: UsageAggregator,
}

/// 从 offset 续读文件新增字节中的完整行，返回 (完整行, 新偏移)。
/// 文件长度 < offset（被截断/重写）时回退为从头读；结尾不足一行的残尾不消费，
/// 偏移停在最后一个换行之后，留待下次续读（写入方是逐行 append 的）。
fn read_new_lines(path: &Path, offset: u64) -> std::io::Result<(Vec<String>, u64)> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let start = if len < offset { 0 } else { offset };
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    // 只消费到最后一个换行：残尾（写入中途的行）下次再读
    let Some(last_nl) = buf.iter().rposition(|b| *b == b'\n') else {
        return Ok((Vec::new(), start));
    };
    let text = String::from_utf8_lossy(&buf[..=last_nl]);
    let lines = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect();
    Ok((lines, start + last_nl as u64 + 1))
}

/// 递归收集 sessions 目录下所有 wire.jsonl（目录不存在/不可读按空处理）
fn collect_wire_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_wire_files(&path, out);
        } else if file_type.is_file() && entry.file_name() == "wire.jsonl" {
            out.push(path);
        }
    }
}

/// 增量扫描实现（目录/时间/时区全部入参化，测试可复现跨天边界）：
/// 读状态 → 续读各文件新行 → 喂聚合 → 落盘状态 → 出统计视图
fn scan_with<Tz: TimeZone>(
    sessions_dir: &Path,
    state_path: &Path,
    now_ms: i64,
    tz: &Tz,
) -> LocalUsageStats
where
    Tz::Offset: std::fmt::Display,
{
    // 时间戳溢出（实际不可能）按空结果容忍，与全模块的派生数据哲学一致
    let Some(now_dt) = tz.timestamp_millis_opt(now_ms).single() else {
        return LocalUsageStats::default();
    };
    let today = now_dt.date_naive();

    let mut state = load_state(state_path);
    state.totals.prune(today);

    // 迁移旧版状态：占比改按日×模型之前，scan-state 只有按日总和（by_date）没有
    // 按日×模型。检测到旧格式时清空累计并强制全量重读，重建 by_date_model——
    // 否则升级当天"今日分模型"会一直空着直到产生新消耗，与今日总量对不上。
    // 已消失会话文件的旧贡献在重扫中不可恢复（派生数据，丢了重扫即可，与模块哲学一致）。
    if state.totals.by_date_model.is_empty() && !state.totals.by_date.is_empty() {
        state.totals.by_date.clear();
        state.files.values_mut().for_each(|offset| *offset = 0);
    }

    let mut files = Vec::new();
    collect_wire_files(sessions_dir, &mut files);
    // 排序保证处理顺序确定（状态落盘内容可复现）
    files.sort();

    // 已消失的文件清掉偏移：同名新文件会从头读，不会按旧偏移跳过开头
    let disk_paths: HashSet<String> = files
        .iter()
        .map(|f| f.to_string_lossy().into_owned())
        .collect();
    state.files.retain(|p, _| disk_paths.contains(p));

    for path in &files {
        let key = path.to_string_lossy().into_owned();
        let offset = state.files.get(&key).copied().unwrap_or(0);
        match read_new_lines(path, offset) {
            Ok((lines, new_offset)) => {
                for line in &lines {
                    if let Some(event) = parse_usage_line(line) {
                        state.totals.add(&event, tz);
                    }
                }
                state.files.insert(key, new_offset);
            }
            // 单文件读失败（占用/权限）跳过：保留旧偏移，下次重试
            Err(_) => continue,
        }
    }

    state.last_scan_at = Some(now_dt.timestamp());
    // 状态只是增量加速用，写失败退化为下次全扫，不影响本次结果
    let _ = save_state(state_path, &state);
    state.totals.finish(today, state.last_scan_at)
}

fn load_state(path: &Path) -> ScanState {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        // 不存在/读失败：空状态，等价于首次全扫
        Err(_) => return ScanState::default(),
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// 原子写 scan-state.json（临时文件 + rename；先删目标再 rename，与 storage::save_json 同款）
fn save_state(path: &Path, state: &ScanState) -> Result<(), String> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| format!("创建配置目录失败: {e}"))?;
    let json = serde_json::to_string_pretty(state).map_err(|e| format!("序列化失败: {e}"))?;
    let tmp_path = dir.join("scan-state.json.tmp");
    std::fs::write(&tmp_path, json).map_err(|e| format!("写入临时文件失败: {e}"))?;
    if path.exists() {
        std::fs::remove_file(path).map_err(|e| format!("删除旧文件失败: {e}"))?;
    }
    std::fs::rename(&tmp_path, path).map_err(|e| format!("重命名临时文件失败: {e}"))
}

/// 解析 __secondary__ 对应的真实模型别名：环境变量 KIMI_SECONDARY_MODEL（非空）优先，
/// 其次 `{home}/.kimi-code/config.toml` 的 `[secondary_model].model`（优先级与 CLI 一致）。
/// 两处都取不到（未开实验 / 配置缺失 / 文件损坏）为 None，哨兵桶原样展示。
/// home 规则与 sessions_dir 一致（USERPROFILE → HOME）；配置里其余字段（api_key 等）
/// 只在内存中解析，不读用不落盘
fn resolve_secondary_model() -> Option<String> {
    if let Ok(value) = std::env::var("KIMI_SECONDARY_MODEL") {
        let value = value.trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    let config_path = PathBuf::from(home).join(".kimi-code").join("config.toml");
    let text = std::fs::read_to_string(config_path).ok()?;
    let doc = text.parse::<toml::Table>().ok()?;
    doc.get("secondary_model")?
        .get("model")?
        .as_str()
        .map(str::to_string)
}

/// 会话根目录：{userprofile}/.kimi-code/sessions（Kimi Code CLI 的会话落盘位置）；
/// 取不到用户目录为 None（scan 按空目录处理）
fn sessions_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|home| PathBuf::from(home).join(".kimi-code").join("sessions"))
}

/// 扫描状态路径：{config_dir}/scan-state.json（config_dir 规则与 storage.rs 一致）
fn state_file_path() -> PathBuf {
    crate::storage::config_dir().join("scan-state.json")
}

/// 导出实现（目录/时间入参化以便单测）：写 CSV + 复制 history.json，返回 exports 目录路径
fn export_report_to<Tz: TimeZone>(
    exports_dir: &Path,
    history_src: &Path,
    points: &[HistoryPoint],
    now: DateTime<Tz>,
) -> Result<PathBuf, String>
where
    Tz::Offset: std::fmt::Display,
{
    std::fs::create_dir_all(exports_dir).map_err(|e| format!("创建导出目录失败: {e}"))?;
    let csv_path = exports_dir.join(format!("usage-{}.csv", now.format("%Y%m%d-%H%M%S")));
    std::fs::write(&csv_path, build_history_csv(points, &now.timezone()))
        .map_err(|e| format!("写入 CSV 失败: {e}"))?;
    // history.json 原文一并复制（排查对数用）；源不存在（从未刷新成功过）跳过
    if history_src.exists() {
        std::fs::copy(history_src, exports_dir.join("history.json"))
            .map_err(|e| format!("复制 history.json 失败: {e}"))?;
    }
    Ok(exports_dir.to_path_buf())
}

/// 由采样点生成 CSV 文本（时区入参化，测试用固定偏移复现本地时间列）：
/// 表头 time,weekly,five_hour,monthly；时间为本地 ISO；None 字段为空单元格
fn build_history_csv<Tz: TimeZone>(points: &[HistoryPoint], tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let mut out = String::from(CSV_HEADER);
    for p in points {
        let time = tz
            .timestamp_opt(p.t, 0)
            .single()
            .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S").to_string())
            .unwrap_or_default();
        out.push('\n');
        out.push_str(&format!(
            "{},{},{},{}",
            time,
            csv_num(p.weekly),
            csv_num(p.five_hour),
            csv_num(p.monthly)
        ));
    }
    out.push('\n');
    out
}

/// Option<f64> → CSV 单元格：None 为空串
fn csv_num(v: Option<f64>) -> String {
    v.map(|v| v.to_string()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

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
            parse_usage_line(r#"{"type":"usage.record","model":"m","usage":{"output":1}}"#)
                .is_none()
        );
    }

    #[test]
    fn parse_defaults_model_and_usage() {
        // 缺 model → "unknown" 桶；缺 usage → 0
        let event = parse_usage_line(r#"{"type":"usage.record","time":1000}"#).unwrap();
        assert_eq!(event.model, "unknown");
        assert_eq!(event.tokens, 0);
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
        let stats = agg.finish(today, Some(123));
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
        };
        let stats = agg.finish(NaiveDate::from_ymd_opt(2026, 7, 27).unwrap(), None);
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
        let stats = scan_with(&sessions, &state_path, now, &tz);
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
        let stats2 = scan_with(&sessions, &state_path, now, &tz);
        assert_eq!(stats2.today_tokens, stats.today_tokens);
        assert_eq!(stats2.by_model, stats.by_model);

        // append 一条今日事件：三次扫描只增量这一条
        let extra = usage_line("kimi-code/k3", "2026-07-27T11:30:00+08:00", 1, 2);
        let mut content = std::fs::read_to_string(&main_file).unwrap();
        content.push_str(&extra);
        content.push('\n');
        std::fs::write(&main_file, content).unwrap();
        let stats3 = scan_with(&sessions, &state_path, now, &tz);
        assert_eq!(stats3.today_tokens, stats.today_tokens + 1 + 2 + 11264);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_migrates_legacy_state_without_by_date_model() {
        let dir = temp_dir("local-usage-migrate");
        let sessions = dir.join("sessions");
        let state_path = dir.join("config").join("scan-state.json");
        let tz = tz8();
        let now = ms("2026-07-27T12:00:00+08:00");

        // 今日一条事件；旧状态偏移已越过它（模拟旧版已消费），
        // 无迁移时该事件不会重读，by_date_model 将一直为空
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
        // 旧版 scan-state：只有 by_date 与文件偏移，没有 by_date_model
        let legacy = serde_json::json!({
            "last_scan_at": ms("2026-07-27T09:00:00+08:00") / 1000,
            "files": { file.to_string_lossy().into_owned(): std::fs::metadata(&file).unwrap().len() },
            "totals": { "by_date": { "2026-07-27": today_tokens } },
        });
        std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        std::fs::write(&state_path, serde_json::to_string(&legacy).unwrap()).unwrap();

        // 迁移触发：累计清空 + 偏移归零全量重读，今日分模型重建
        let stats = scan_with(&sessions, &state_path, now, &tz);
        assert_eq!(stats.today_tokens, today_tokens);
        assert_eq!(stats.by_model.len(), 1);
        assert_eq!(stats.by_model[0].model, "kimi-code/k3");
        assert_eq!(stats.by_model[0].tokens, today_tokens);

        // 迁移只发生一次：二次扫描不再重读，不重复计数
        let stats2 = scan_with(&sessions, &state_path, now, &tz);
        assert_eq!(stats2.today_tokens, today_tokens);
        assert_eq!(stats2.by_model, stats.by_model);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_missing_sessions_dir_returns_empty() {
        let dir = temp_dir("local-usage-empty");
        let stats = scan_with(
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

        // 今日事件（用真实本地时钟，scan() 走 chrono::Local）
        let now_ms = chrono::Local::now().timestamp_millis();
        let line = format!(
            r#"{{"type":"usage.record","model":"kimi-code/k3","usage":{{"inputOther":1,"output":2,"inputCacheRead":0,"inputCacheCreation":0}},"usageScope":"turn","time":{now_ms}}}"#
        );
        write_wire(&home.join(".kimi-code").join("sessions"), "main", &[line]);

        let stats1 = scan();
        assert_eq!(stats1.today_tokens, 3);
        assert!(config.join("scan-state.json").exists());

        // 距上次 < 180 秒：直接返回缓存（last_scan_at 相同即未重扫）
        let stats2 = scan();
        assert_eq!(stats2, stats1);

        std::env::remove_var("USERPROFILE");
        std::env::remove_var("KIMICODEBAR_CONFIG_DIR");
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
        let out = export_report_to(&exports, &history_src, &points, now).unwrap();
        assert_eq!(out, exports);

        // CSV 文件名带本地时间戳，内容表头 + 一行
        let csv_path = exports.join("usage-20260727-123456.csv");
        let csv = std::fs::read_to_string(&csv_path).unwrap();
        assert_eq!(
            csv,
            "time,weekly,five_hour,monthly\n2026-07-27T12:34:56,42.5,,\n"
        );
        // history.json 原文已复制到同目录
        assert_eq!(
            std::fs::read_to_string(exports.join("history.json")).unwrap(),
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
        export_report_to(&exports, &dir.join("history.json"), &[], now).unwrap();
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

        let mut stats = scan_with(&sessions, &state_path, now, &tz);
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
}
