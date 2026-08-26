//! 本地 Token 消耗统计：增量扫描 Kimi Code 会话的 wire.jsonl 用量事件，
//! 聚合为今日/昨日/最近 7 天/分模型累计（语义移植自 macOS 版 KimiLocalUsage.swift）。
//!
//! 数据源：枚举全部 CLI home（默认 `{userprofile}/.kimi-code` + glob `.kimi-code-*`，
//! 见 cli_homes；另含 WSL 侧 home——发行版名单读注册表 Lxss 键，经 `\\wsl.localhost`
//! 枚举各发行版 home/ 下用户目录与 root/ 的 .kimi-code，见 wsl_homes），递归遍历
//! 各 home 的 `sessions/**/wire.jsonl`，逐行 JSON，
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
//! 增量扫描：`{config_dir}/scan-state.json` 记录每个文件的已读字节偏移与分账号累计聚合，
//! 每次只读各文件偏移之后的新字节；文件被截断/重写（长度 < 偏移）回退为从头读。
//! 状态全量原子写（临时文件 + rename，与 storage.rs 同款）。
//! 扫描节流：进程内缓存结果（ScanView），距上次扫描 < 180 秒直接返回缓存。
//!
//! 分账号归属：每次增量扫描按 home 各快照一次 CLI 凭证（snapshot_attribution(home)），
//! 该 home 的新事件按自己 home 的快照归入对应桶
//! （键 = 账号 id；比对全不中的进 "unassigned" 未归属桶，不做任何 UI 展示）：
//! - 模型路由：先查该 home config.toml 的 [models] 表拿 provider（含 "kimi" → Kimi 路由，
//!   覆盖 "managed:kimi-code"；"deepseek" 开头 → DeepSeek；dashscope 等第三方 → 未归属）；
//!   查不到按前缀兜底——deepseek 开头 → DeepSeek，其余 → Kimi；
//! - Kimi 路由：该 home 的 kimi api_key 与各 Kimi 账号登记的任一 key（主 key 或
//!   任一额外 key，见 creds.rs 的 api_key_extra 槽位）精确相等 → 归该账号；
//!   否则解该 home OAuth access_token（JWT）的 user_id（缺失退 sub）与各账号 OAuth token 的
//!   user_id 比对，相等 → 归该账号；都不中 → 未归属；
//! - DeepSeek 路由：CLI 的 deepseek api_key 与各 DeepSeek 账号登记的任一 key
//!   （主 key 或任一额外 key）精确相等 → 归该账号，否则未归属；
//! - 归属判定时机 = 扫描时快照：扫描 ≤3 分钟一轮，换号存在对应误差窗（拍板接受，
//!   不保证逐条精确）；JWT 只解 payload 不验签、不联网；
//! - CLI 与各账号的 key / user_id 只在内存比对，绝不落盘进 scan-state.json。
//!
//! 与 macOS 原版的已知差异（原版仓库不在本机，按钉死的契约语义实现）：
//! - daily 固定输出最近 7 个自然日（无消耗的日子补 0），保证前端折线图逐日连续；
//! - 按日×模型的累计聚合随偏移一起持久化在 scan-state.json：
//!   增量读取下"今日分模型 by_model"必须靠落盘的按日×模型累计值，否则每次都得全量重读；
//!   by_model 语义为今日（与卡片主体"今日/近 7 天"一致），不是全部时间累计；
//! - 旧版状态（机器级 totals 合计、无 buckets 键）下次扫描时整体丢弃：清空聚合 +
//!   全部文件偏移归零全量重扫（拍板：旧合计不做任何保留）；
//! - 已删账号的桶不主动清，30 天保留窗口自然衰减；
//! - 文件截断回退为整文件重读，该文件的旧贡献理论上可能重复计数一次
//!   （会话文件按 uuid 命名、只增不改，实际不会触发）；
//! - 已消失文件的偏移会被清理，同名新文件从头读，不会按旧偏移跳过开头。
//!
//! 跨 Harness 扩展（Claude Code / Codex / OpenCode 三家本地日志，解析实现见
//! local_usage/ 子模块）：三家日志与 Kimi home 并列喂同一套分桶聚合器，事件语义
//! 不变（ts 毫秒 / model / tokens），UI 零改动。归属：按各家配置文件里的 API key
//! 与**全部账号（不分 provider）**登记的任一 key（keyring 主 key 或任一额外 key）
//! 精确相等 → 归该账号；
//! 取不到 key 或全不中进未归属桶（OAuth 形态登录设计内落此桶）。扫描状态新增
//! 键（Claude 的 message.id 去重集、Codex 的文件级模型/累计、OpenCode 的
//! time_created 水位 + id 去重集）全走 serde(default)，旧 scan-state 兼容不清零；
//! 去重集按 48 小时裁剪防膨胀。新 harness 事件同样计入机器级活跃判定。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, NaiveDate, TimeZone};
use serde::{Deserialize, Serialize};

use crate::history::HistoryPoint;

mod claude;
mod codex;
mod opencode;

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
/// 跨 Harness 去重集（Claude message.id / OpenCode 消息 id）的保留窗口（毫秒）
const HARNESS_DEDUP_MS: i64 = 48 * 3600 * 1000;
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

/// 单账号的本地 token 消耗统计（get_local_usage 的返回，与 types.ts LocalUsageStats 一一对应）
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
    /// 该账号最近一次 usage.record 事件时间（epoch 毫秒），从未扫到为 null；
    /// 机器级活跃判定位在 ScanView.machine_last_event_at
    pub last_event_at: Option<i64>,
}

/// 扫描结果视图（scan 的返回）：机器级最近事件时间 + 各桶统计。
/// by_account 的键 = 账号 id 或未归属桶 UNASSIGNED_BUCKET（UI 只按账号 id 取，
/// 未归属数字不出现在任何页面）
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScanView {
    /// 全部桶（含未归属）最近 usage.record 事件时间的 max（epoch 毫秒）；
    /// 机器级语义，自适应刷新的活跃判定依据（polling.rs），与旧版机器级 last_event_at 等价
    pub machine_last_event_at: Option<i64>,
    /// 桶键（账号 id / 未归属）→ 该桶的统计视图
    pub by_account: HashMap<String, LocalUsageStats>,
    /// 上次扫描时间（epoch 秒），未扫过为 null
    pub last_scan_at: Option<i64>,
    /// 无桶账号的空统计模板：daily 补全最近 7 天零值、last_scan_at 照填（诚实零）
    pub empty: LocalUsageStats,
}

impl ScanView {
    /// 取某账号的统计：无桶（该账号从未归属到消耗）给空统计模板（7 天零值，last_scan_at 照填）
    pub fn for_account(&self, account_id: &str) -> LocalUsageStats {
        self.by_account
            .get(account_id)
            .cloned()
            .unwrap_or_else(|| self.empty.clone())
    }
}

/// 进程内结果缓存（节流用）：上次扫描完成时刻（epoch 秒）+ 结果
static SCAN_CACHE: Mutex<Option<(i64, ScanView)>> = Mutex::new(None);

/// 扫描一次本地用量：距上次 < 180 秒返回进程内缓存，否则增量扫描并落盘状态。
/// 永不失败：sessions 目录不存在、单文件读失败、状态写失败、凭证快照读取失败
/// 均容忍为（部分）空结果 —— 与 history 一致，统计是派生数据，丢了重扫即可。
pub fn scan() -> ScanView {
    let now = chrono::Local::now();
    let now_secs = now.timestamp();
    {
        let cache = SCAN_CACHE.lock().unwrap();
        if let Some((scanned_at, view)) = &*cache {
            if now_secs - *scanned_at < THROTTLE_SECS {
                return view.clone();
            }
        }
    }
    let view = scan_fresh(now.timestamp_millis(), &chrono::Local);
    *SCAN_CACHE.lock().unwrap() = Some((now_secs, view.clone()));
    view
}

/// 不节流的完整扫描（scan 去掉进程内缓存的部分，测试可直接驱动）：
/// 枚举全部 CLI home → 每个 home 用自己的凭证快照 → 逐 home 增量扫描；
/// 另采集三家 harness（Claude Code / Codex / OpenCode）的扫描输入一并扫。
/// home 枚举失败（取不到用户目录）容忍为空目标，按空结果扫描
fn scan_fresh<Tz: TimeZone>(now_ms: i64, tz: &Tz) -> ScanView
where
    Tz::Offset: std::fmt::Display,
{
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    let mut targets: Vec<(PathBuf, Attribution)> = home
        .as_deref()
        .map(Path::new)
        .map(cli_homes)
        .unwrap_or_default()
        .into_iter()
        .map(|h| {
            // 该 home 的事件用该 home 的快照归属（凭证三处全在该 home 内）
            let attribution = snapshot_attribution(&h);
            (h.join("sessions"), attribution)
        })
        .collect();
    // WSL 侧 home 并列进扫描目标：与本地 home 同一节流节奏，各用自己 home 的
    // 凭证快照归属（同账号自然归同桶）；WSL 未装/关机时 wsl_homes 为空，无特殊分支
    for wsl_home in wsl_homes() {
        let attribution = snapshot_attribution(&wsl_home);
        targets.push((wsl_home.join("sessions"), attribution));
    }
    let harness = harness_input(home.as_deref());
    let mut view = scan_full(&targets, &harness, &state_file_path(), now_ms, tz);
    // __secondary__ 桶并入真实副模型（展示层折叠，不落盘；解析不到保留原样）：逐桶折叠
    if let Some(target) = resolve_secondary_model() {
        for stats in view.by_account.values_mut() {
            fold_secondary_model(&mut stats.by_model, &target);
        }
    }
    view
}

/// 三家 harness 的扫描输入（根目录与账号 key 快照，测试可整体伪造绕开环境解析）：
/// - Claude / Codex 只认默认路径（CLAUDE_CONFIG_DIR / CODEX_HOME 指走的 home
///   探测不到，拍板接受）；
/// - OpenCode 候选目录存在几个扫几个（按优先级去重）；
/// - key_accounts = 全部账号（不分 provider）的 api_key 快照：harness 事件按
///   key 精确匹配归属，只在内存比对、不落盘
#[derive(Default)]
struct HarnessInput {
    claude_dir: Option<PathBuf>,
    codex_dir: Option<PathBuf>,
    opencode_data_dirs: Vec<PathBuf>,
    opencode_config_dirs: Vec<PathBuf>,
    /// (api_key, 账号 id)
    key_accounts: Vec<(String, String)>,
}

/// 从环境解析三家 harness 的扫描输入（归属 key 的采集在扫描函数内做）
fn harness_input(home: Option<&std::ffi::OsStr>) -> HarnessInput {
    let home = home.map(Path::new);
    let mut input = HarnessInput {
        claude_dir: home.map(|h| h.join(".claude")),
        codex_dir: home.map(|h| h.join(".codex")),
        ..HarnessInput::default()
    };
    let mut data_dirs: Vec<PathBuf> = Vec::new();
    push_existing(
        &mut data_dirs,
        std::env::var_os("XDG_DATA_HOME").map(|p| PathBuf::from(p).join("opencode")),
    );
    if let Some(h) = home {
        push_existing(
            &mut data_dirs,
            Some(h.join(".local").join("share").join("opencode")),
        );
    }
    push_existing(
        &mut data_dirs,
        std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("opencode")),
    );
    push_existing(
        &mut data_dirs,
        std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("opencode")),
    );
    input.opencode_data_dirs = data_dirs;

    let mut config_dirs: Vec<PathBuf> = Vec::new();
    push_existing(
        &mut config_dirs,
        std::env::var_os("XDG_CONFIG_HOME").map(|p| PathBuf::from(p).join("opencode")),
    );
    if let Some(h) = home {
        push_existing(&mut config_dirs, Some(h.join(".config").join("opencode")));
    }
    push_existing(
        &mut config_dirs,
        std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("opencode")),
    );
    input.opencode_config_dirs = config_dirs;

    // 全部账号（不分 provider）的 api_key：harness 归属比对的账号侧快照。
    // 主 key 与每把额外 key 各自成一条目，命中任一即归该账号
    for account in &crate::storage::load_settings().unwrap_or_default().accounts {
        if let Some(key) = crate::creds::load_api_key(&account.id)
            .ok()
            .flatten()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
        {
            input.key_accounts.push((key, account.id.clone()));
        }
        for key in crate::creds::load_api_key_extra(&account.id).unwrap_or_default() {
            let key = key.trim().to_string();
            if !key.is_empty() {
                input.key_accounts.push((key, account.id.clone()));
            }
        }
    }
    input
}

/// 候选目录去重入表：仅收存在目录（opencode.db / auth.json 的存在性后续读取时判）
fn push_existing(dirs: &mut Vec<PathBuf>, candidate: Option<PathBuf>) {
    if let Some(path) = candidate {
        if path.is_dir() && !dirs.contains(&path) {
            dirs.push(path);
        }
    }
}

/// 导出用量报告：每个账号的历史采样各写一个 CSV 到 `{config_dir}/exports/`
/// （`usage-YYYYMMDD-HHmmss-<账号名>.csv`），并把对应 history-<id>.json 原文复制到同目录；
/// 返回 exports 目录路径（reveal 由命令层负责）
pub fn export_usage_report() -> Result<PathBuf, String> {
    let config_dir = crate::storage::config_dir();
    let settings = crate::storage::load_settings().unwrap_or_default();
    let exports_dir = config_dir.join("exports");
    let now = chrono::Local::now();
    let mut any = false;
    for account in &settings.accounts {
        let points = crate::history::HistoryStore::load(&account.id).into_points();
        let history_src = config_dir.join(format!("history-{}.json", account.id));
        if points.is_empty() && !history_src.exists() {
            continue;
        }
        export_report_to(
            &exports_dir,
            &history_src,
            &points,
            now,
            Some(&account.name),
        )?;
        any = true;
    }
    if !any {
        // 无账号或全无历史：仍产出一个空 CSV（保持旧版"空历史也导出空表"的行为）
        export_report_to(
            &exports_dir,
            &config_dir.join("history.json"),
            &[],
            now,
            None,
        )?;
    }
    Ok(exports_dir)
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
    /// 见过的最近一条 usage.record 事件时间（epoch 毫秒，单调取 max）；
    /// 不受按日窗口裁剪影响，自适应刷新靠它判"近 10 分钟有无新消耗"
    #[serde(default)]
    last_event_at: Option<i64>,
}

impl UsageAggregator {
    /// 喂入一条事件：按本地日期与按日×模型分别累计。
    /// 日期键取不出（时间戳溢出）时丢弃该事件（与解析层缺 time 同策略）
    fn add<Tz: TimeZone>(&mut self, event: &UsageEvent, tz: &Tz)
    where
        Tz::Offset: std::fmt::Display,
    {
        // 活跃判定时钟与日期分桶无关：时间戳合法即更新 max（溢出值比较无害）
        self.last_event_at = Some(
            self.last_event_at
                .map_or(event.ts_ms, |old| old.max(event.ts_ms)),
        );
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
            last_event_at: self.last_event_at,
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

/// 扫描状态（scan-state.json）：文件偏移 + 分桶累计聚合。损坏/不存在容忍为空状态重新全扫
#[derive(Debug, Default, Serialize, Deserialize)]
struct ScanState {
    /// 上次完成扫描时间（epoch 秒）
    #[serde(default)]
    last_scan_at: Option<i64>,
    /// 文件路径 → 已读字节偏移（Kimi wire.jsonl + Claude/Codex 的 jsonl 共用）
    #[serde(default)]
    files: HashMap<String, u64>,
    /// 分桶累计聚合：键 = 账号 id，未归属桶键为 UNASSIGNED_BUCKET
    /// （增量读取下全时间统计的来源；旧版机器级 totals 合计见 load_state 的迁移）
    #[serde(default)]
    buckets: HashMap<String, UsageAggregator>,
    /// Claude message.id → 已计入（跨文件全局去重：resume 会话文件会复制旧消息）
    #[serde(default)]
    claude_ids: HashMap<String, ClaudeIdEntry>,
    /// Codex 文件路径 → 最近 turn_context 模型（增量续扫下跨批次记忆）
    #[serde(default)]
    codex_models: HashMap<String, String>,
    /// Codex 文件路径 → 上次 total_token_usage 累计（差分基线）
    #[serde(default)]
    codex_totals: HashMap<String, CodexTotals>,
    /// OpenCode 数据目录 → 扫描水位与已计消息 id
    #[serde(default)]
    opencode: HashMap<String, OpenCodeDbState>,
}

/// Claude message.id 去重条目：已计入 tokens + 最近见到的时间（48h 裁剪依据）
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct ClaudeIdEntry {
    #[serde(default)]
    tokens: u64,
    #[serde(default)]
    seen_ms: i64,
}

/// Codex 文件级累计快照（total_token_usage 差分基线；分量取 max 推进防快照回退）
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct CodexTotals {
    #[serde(default)]
    input: u64,
    #[serde(default)]
    cached: u64,
    #[serde(default)]
    output: u64,
}

impl CodexTotals {
    /// 当前值相对上次基线的正向差（负差按 0 丢弃）
    fn diff(&self, prev: &CodexTotals) -> u64 {
        self.input.saturating_sub(prev.input)
            + self.cached.saturating_sub(prev.cached)
            + self.output.saturating_sub(prev.output)
    }

    /// 基线按分量 max 推进（限流刷新重发旧快照不让基线回退）
    fn merge_max(&mut self, other: &CodexTotals) {
        self.input = self.input.max(other.input);
        self.cached = self.cached.max(other.cached);
        self.output = self.output.max(other.output);
    }
}

/// OpenCode 单库扫描状态：time_created 水位 + 已计消息 id → 其时间戳（48h 裁剪）
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct OpenCodeDbState {
    #[serde(default)]
    watermark_ms: i64,
    #[serde(default)]
    ids: HashMap<String, i64>,
}

// ---------------------------------------------------------------------------
// 分账号归属（纯函数 + 凭证快照入参化，可直接单测；快照实现见 snapshot_attribution）
// ---------------------------------------------------------------------------

/// 未归属桶的键：凭证比对全不中 / 第三方路由的事件进此桶，不做任何 UI 展示。
/// pub(crate)：statusline 归属判定要比较返回值是否落入此桶
pub(crate) const UNASSIGNED_BUCKET: &str = "unassigned";

/// 归属路由（模型 → 哪条凭证比对通道）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    Kimi,
    DeepSeek,
    /// 第三方 provider（dashscope 等）：直接未归属，不比凭证
    Unassigned,
}

/// CLI 侧凭证快照（每次增量扫描开头读一次，本批新事件统一按它归属）。
/// 只在内存参与比对，绝不落盘进 scan-state.json
#[derive(Debug, Default, Clone, PartialEq)]
struct CliCredentials {
    /// config.toml [providers."managed:kimi-code"] 的 api_key（空白按未配置）
    kimi_api_key: Option<String>,
    /// credentials/kimi-code.json 的 access_token（JWT）解出的 user_id（缺失退 sub；过期也能解）
    kimi_user_id: Option<String>,
    /// config.toml [providers.deepseek] 的 api_key（空白按未配置）
    deepseek_api_key: Option<String>,
}

/// 账号侧凭证快照（比对用，只在内存）
#[derive(Debug, Default, Clone, PartialEq)]
struct AccountCreds {
    /// creds::load_api_key（空白按未配置）
    api_key: Option<String>,
    /// OAuth access_token（JWT）解出的 user_id（仅 Kimi 账号有意义）
    user_id: Option<String>,
    /// creds::load_api_key_extra（额外 key，与主 key 同权参与归属；只在内存比对）
    extra_api_keys: Vec<String>,
}

/// 一次扫描的归属上下文：CLI 凭证快照 + 各账号凭证快照 + 模型路由表。
/// pub(crate)：statusline 的账号解析（statusline.rs）经由 snapshot_attribution /
/// attribute_cli 复用，类型需与这两个 pub(crate) 函数同可见
#[derive(Debug, Default, Clone)]
pub(crate) struct Attribution {
    cli: CliCredentials,
    /// (账号 id, 凭证)，Kimi 账号
    kimi_accounts: Vec<(String, AccountCreds)>,
    /// (账号 id, 凭证)，DeepSeek 账号
    deepseek_accounts: Vec<(String, AccountCreds)>,
    /// config.toml [models] 表：模型名 → provider（如 "managed:kimi-code" / "deepseek" / "dashscope"）
    model_providers: HashMap<String, String>,
}

/// JWT payload 的 user_id（缺失退 sub）：取第二段 base64url（无填充）解 JSON。
/// 不验签、不联网；任何一步失败（段数不够 / 解码失败 / 非 JSON / 字段缺失或非字符串）按 None 容忍
fn jwt_user_id(token: &str) -> Option<String> {
    use base64::Engine;
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    ["user_id", "sub"]
        .iter()
        .find_map(|key| value.get(key)?.as_str().map(str::to_string))
}

/// 模型路由：先查 [models] 表拿 provider——含 "kimi" → Kimi 路由（覆盖 "managed:kimi-code"），
/// "deepseek" 开头 → DeepSeek，其余第三方（dashscope 等）→ 未归属；
/// 查不到按前缀兜底——deepseek 开头 → DeepSeek，其余 → Kimi
fn route_model(model: &str, attribution: &Attribution) -> Route {
    match attribution.model_providers.get(model) {
        Some(provider) if provider.contains("kimi") => Route::Kimi,
        Some(provider) if provider.starts_with("deepseek") => Route::DeepSeek,
        Some(_) => Route::Unassigned,
        None if model.starts_with("deepseek") => Route::DeepSeek,
        None => Route::Kimi,
    }
}

/// 该账号登记的 key 集合（主 key + 全部额外 key）是否含给定 key（精确相等）
fn account_has_key(creds: &AccountCreds, key: &str) -> bool {
    creds.api_key.as_deref() == Some(key) || creds.extra_api_keys.iter().any(|k| k == key)
}

/// 单条事件的归属桶键：按路由把 CLI 快照与各账号凭证做精确比对，全不中 → 未归属。
/// kimi 侧 api_key（主或任一额外）先比、OAuth user_id 后比（都是精确匹配，顺序无副作用）
fn attribute(model: &str, attribution: &Attribution) -> String {
    match route_model(model, attribution) {
        Route::Kimi => {
            if let Some(key) = &attribution.cli.kimi_api_key {
                if let Some((id, _)) = attribution
                    .kimi_accounts
                    .iter()
                    .find(|(_, creds)| account_has_key(creds, key))
                {
                    return id.clone();
                }
            }
            if let Some(user_id) = &attribution.cli.kimi_user_id {
                if let Some((id, _)) = attribution
                    .kimi_accounts
                    .iter()
                    .find(|(_, creds)| creds.user_id.as_deref() == Some(user_id))
                {
                    return id.clone();
                }
            }
            UNASSIGNED_BUCKET.to_string()
        }
        Route::DeepSeek => {
            if let Some(key) = &attribution.cli.deepseek_api_key {
                if let Some((id, _)) = attribution
                    .deepseek_accounts
                    .iter()
                    .find(|(_, creds)| account_has_key(creds, key))
                {
                    return id.clone();
                }
            }
            UNASSIGNED_BUCKET.to_string()
        }
        Route::Unassigned => UNASSIGNED_BUCKET.to_string(),
    }
}

/// statusline 专用归属（pub(crate)：statusline.rs 的账号解析用它）：
/// statusline 进程没有事件模型可路由，直接按 CLI 侧凭证通道判定——
/// 优先 Kimi 通道（api_key 比对 → OAuth user_id 比对；GLM key 也在
/// managed:kimi-code 槽位，同走此通道）；未命中且 CLI 配了 deepseek key
/// 再走 DeepSeek 通道兜底（一个 home 双 provider 的场景）；全不中返回未归属桶键
pub(crate) fn attribute_cli(attribution: &Attribution) -> String {
    let kimi_bucket = attribute("managed:kimi-code", attribution);
    if kimi_bucket != UNASSIGNED_BUCKET || attribution.cli.deepseek_api_key.is_none() {
        return kimi_bucket;
    }
    attribute("deepseek", attribution)
}

/// 归属上下文快照：指定 CLI home 的凭证三处（该 home 的 config.toml 的 kimi/deepseek
/// api_key 与 [models] 路由表、credentials/kimi-code.json 的 OAuth user_id）+ 各账号凭证
/// （keyring 主 key + 额外 key / OAuth user_id，与 home 无关，逐 home 快照时每次重读）。
/// 所有读取失败（文件缺失/损坏、keyring 错误、凭证未配置）一律容忍为空——扫描永不失败；
/// 某 home 快照为空时该 home 的事件全部进未归属桶，机器级活跃判定不受影响。
/// pub(crate)：statusline 的账号解析（statusline.rs）要复用同一份快照
pub(crate) fn snapshot_attribution(home: &Path) -> Attribution {
    let mut attribution = Attribution::default();
    if let Ok(text) = std::fs::read_to_string(home.join("config.toml")) {
        if let Ok(doc) = text.parse::<toml::Table>() {
            let provider_key = |name: &str| {
                doc.get("providers")?
                    .get(name)?
                    .get("api_key")?
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            attribution.cli.kimi_api_key = provider_key("managed:kimi-code");
            attribution.cli.deepseek_api_key = provider_key("deepseek");
            if let Some(models) = doc.get("models").and_then(|m| m.as_table()) {
                for (name, def) in models {
                    if let Some(provider) = def.get("provider").and_then(|p| p.as_str()) {
                        attribution
                            .model_providers
                            .insert(name.clone(), provider.to_string());
                    }
                }
            }
        }
    }
    if let Ok(text) = std::fs::read_to_string(home.join("credentials").join("kimi-code.json")) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
            attribution.cli.kimi_user_id = json
                .get("access_token")
                .and_then(|t| t.as_str())
                .and_then(jwt_user_id);
        }
    }
    for account in &crate::storage::load_settings().unwrap_or_default().accounts {
        let api_key = crate::creds::load_api_key(&account.id)
            .ok()
            .flatten()
            .map(|k| k.trim().to_string())
            .filter(|s| !s.is_empty());
        // 额外 key（trim + 滤空）：读取失败容忍为空数组，与主 key 同权参与归属
        let extra_api_keys = crate::creds::load_api_key_extra(&account.id)
            .unwrap_or_default()
            .into_iter()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .collect();
        if account.is_deepseek() {
            attribution.deepseek_accounts.push((
                account.id.clone(),
                AccountCreds {
                    api_key,
                    user_id: None,
                    extra_api_keys,
                },
            ));
        } else {
            let user_id = crate::kimi::oauth::load_credentials(&account.id)
                .ok()
                .flatten()
                .and_then(|creds| jwt_user_id(&creds.access_token));
            attribution.kimi_accounts.push((
                account.id.clone(),
                AccountCreds {
                    api_key,
                    user_id,
                    extra_api_keys,
                },
            ));
        }
    }
    attribution
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

/// 既有 Kimi-only 扫描入口（测试兼容壳）：等价 harness 输入为空的 scan_full
#[cfg(test)]
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

/// scan_with 的完整形态：Kimi home 之外并列扫描三家 harness（输入入参化）。
/// harness 事件按扫描开头的 key 快照归属：事件携带的 key 与全部账号（不分
/// provider）的 api_key 精确相等 → 归该账号，取不到/全不中 → 未归属桶。
/// Kimi 路径与 scan_with 空输入时逐字节等价（行为不变）
fn scan_full<Tz: TimeZone>(
    scan_targets: &[(PathBuf, Attribution)],
    harness: &HarnessInput,
    state_path: &Path,
    now_ms: i64,
    tz: &Tz,
) -> ScanView
where
    Tz::Offset: std::fmt::Display,
{
    // 时间戳溢出（实际不可能）按空结果容忍，与全模块的派生数据哲学一致
    let Some(now_dt) = tz.timestamp_millis_opt(now_ms).single() else {
        return ScanView::default();
    };
    let today = now_dt.date_naive();

    let mut state = load_state(state_path);
    for aggregator in state.buckets.values_mut() {
        aggregator.prune(today);
    }

    // 逐 home 收集 wire 文件，文件带着所属 home 的归属快照走（该 home 的事件按它归属）
    let mut files: Vec<(PathBuf, &Attribution)> = Vec::new();
    for (sessions_dir, attribution) in scan_targets {
        let mut home_files = Vec::new();
        collect_wire_files(sessions_dir, &mut home_files);
        // 排序保证处理顺序确定（状态落盘内容可复现）
        home_files.sort();
        files.extend(home_files.into_iter().map(|file| (file, attribution)));
    }

    // ---- 三家 harness：文件发现与归属 key 快照（扫描开头一次）----
    let claude_key = harness.claude_dir.as_deref().and_then(claude::auth_token);
    let claude_files = harness
        .claude_dir
        .as_deref()
        .map(claude::collect_files)
        .unwrap_or_default();
    let codex_keys = harness
        .codex_dir
        .as_deref()
        .map(codex::auth_keys)
        .unwrap_or_default();
    let codex_files = harness
        .codex_dir
        .as_deref()
        .map(codex::collect_files)
        .unwrap_or_default();
    // auth.json 实际在数据目录（~/.local/share/opencode/，实机踩坑：只查配置目录会
    // 漏 key 全落未归属），opencode.json 在配置目录——两处候选合并喂入，数据目录优先
    let opencode_key_dirs: Vec<PathBuf> = harness
        .opencode_data_dirs
        .iter()
        .chain(harness.opencode_config_dirs.iter())
        .cloned()
        .collect();
    let opencode_keys = opencode::provider_keys(&opencode_key_dirs);
    // (事件, 归属 key)；Claude/Codex 全库一把 key，OpenCode 按消息 providerID 查
    let mut harness_events: Vec<(UsageEvent, Option<String>)> = Vec::new();

    // 已消失的文件清掉偏移：同名新文件会从头读，不会按旧偏移跳过开头
    // （Kimi wire.jsonl 与 Claude/Codex jsonl 的偏移同住一张表，清理统一做）
    let mut disk_paths: HashSet<String> = files
        .iter()
        .map(|(file, _)| file.to_string_lossy().into_owned())
        .collect();
    disk_paths.extend(
        claude_files
            .iter()
            .map(|f| f.to_string_lossy().into_owned()),
    );
    disk_paths.extend(codex_files.iter().map(|f| f.to_string_lossy().into_owned()));
    state.files.retain(|p, _| disk_paths.contains(p));
    state.codex_models.retain(|p, _| disk_paths.contains(p));
    state.codex_totals.retain(|p, _| disk_paths.contains(p));

    for (path, attribution) in &files {
        let key = path.to_string_lossy().into_owned();
        let offset = state.files.get(&key).copied().unwrap_or(0);
        match read_new_lines(path, offset) {
            Ok((lines, new_offset)) => {
                for line in &lines {
                    if let Some(event) = parse_usage_line(line) {
                        let bucket = attribute(&event.model, attribution);
                        state.buckets.entry(bucket).or_default().add(&event, tz);
                    }
                }
                state.files.insert(key, new_offset);
            }
            // 单文件读失败（占用/权限）跳过：保留旧偏移，下次重试
            Err(_) => continue,
        }
    }

    // Claude：续读 → message.id 去重差分出账，整批共用 harness key
    for path in &claude_files {
        let key = path.to_string_lossy().into_owned();
        let offset = state.files.get(&key).copied().unwrap_or(0);
        if let Ok((lines, new_offset)) = read_new_lines(path, offset) {
            for event in claude::settle_new_lines(&lines, &mut state.claude_ids) {
                harness_events.push((event, claude_key.clone()));
            }
            state.files.insert(key, new_offset);
        }
    }

    // Codex：续读 → 文件级模型/累计差分出账。多把候选 key（OPENAI_API_KEY /
    // bearer_token）任一命中账号即归：命中 key 扫描开头判一次，整批共用
    let codex_key = codex_keys
        .iter()
        .find(|key| {
            harness
                .key_accounts
                .iter()
                .any(|(account_key, _)| account_key == *key)
        })
        .cloned();
    for path in &codex_files {
        let key = path.to_string_lossy().into_owned();
        let offset = state.files.get(&key).copied().unwrap_or(0);
        if let Ok((lines, new_offset)) = read_new_lines(path, offset) {
            let mut model = state.codex_models.get(&key).cloned();
            let totals = state.codex_totals.entry(key.clone()).or_default();
            for event in codex::settle_new_lines(&lines, &mut model, totals) {
                harness_events.push((event, codex_key.clone()));
            }
            if let Some(model) = model {
                state.codex_models.insert(key.clone(), model);
            }
            state.files.insert(key, new_offset);
        }
    }

    // OpenCode：逐候选库只读扫描（水位 + id 去重），按消息 providerID 查 key
    for dir in &harness.opencode_data_dirs {
        let db_path = dir.join("opencode.db");
        let dir_key = dir.to_string_lossy().into_owned();
        if !db_path.is_file() {
            state.opencode.remove(&dir_key);
            continue;
        }
        let db_state = state.opencode.entry(dir_key).or_default();
        for (event, provider) in opencode::scan_db(&db_path, db_state) {
            let key = provider
                .as_ref()
                .and_then(|p| opencode_keys.get(p).cloned());
            harness_events.push((event, key));
        }
    }

    // harness 事件入桶：key 与账号 api_key 精确相等 → 该账号；否则未归属
    for (event, key) in &harness_events {
        let bucket = key
            .as_deref()
            .and_then(|k| {
                harness
                    .key_accounts
                    .iter()
                    .find(|(account_key, _)| account_key == k)
                    .map(|(_, id)| id.clone())
            })
            .unwrap_or_else(|| UNASSIGNED_BUCKET.to_string());
        state.buckets.entry(bucket).or_default().add(event, tz);
    }

    // 跨 harness 去重集按 48 小时裁剪（防状态膨胀；会话生命周期内足够兜住重复写）
    let dedup_cutoff = now_ms.saturating_sub(HARNESS_DEDUP_MS);
    state
        .claude_ids
        .retain(|_, entry| entry.seen_ms >= dedup_cutoff);
    for db_state in state.opencode.values_mut() {
        db_state.ids.retain(|_, ts| *ts >= dedup_cutoff);
    }

    state.last_scan_at = Some(now_dt.timestamp());
    // 状态只是增量加速用，写失败退化为下次全扫，不影响本次结果
    let _ = save_state(state_path, &state);

    // 机器级最近事件时间 = 全部桶（含未归属）的 max（polling 活跃判定语义不变）
    let machine_last_event_at = state
        .buckets
        .values()
        .filter_map(|agg| agg.last_event_at)
        .max();
    let by_account = state
        .buckets
        .iter()
        .map(|(key, agg)| (key.clone(), agg.finish(today, state.last_scan_at)))
        .collect();
    ScanView {
        machine_last_event_at,
        by_account,
        last_scan_at: state.last_scan_at,
        // 空聚合器出 7 天零值模板：无桶账号页显示诚实零（daily 逐日连续契约不破）
        empty: UsageAggregator::default().finish(today, state.last_scan_at),
    }
}

/// 读扫描状态：文件不存在/损坏 → 空状态（等价首次全扫）。
/// 旧版状态（机器级 totals 合计、无 buckets 键）整体丢弃：返回空状态即
/// 「清空聚合 + 全部文件偏移归零」，本次扫描全量重读重建分桶（拍板：旧合计不做任何保留）
fn load_state(path: &Path) -> ScanState {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => return ScanState::default(),
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return ScanState::default();
    };
    if value.get("buckets").is_none() {
        return ScanState::default();
    }
    serde_json::from_value(value).unwrap_or_default()
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
/// 其次**默认 home** `{home}/.kimi-code/config.toml` 的 `[secondary_model].model`
/// （优先级与 CLI 一致；拍板：多 home 各配不同副模型的场景不处理，只看默认 home）。
/// 两处都取不到（未开实验 / 配置缺失 / 文件损坏）为 None，哨兵桶原样展示。
/// home 规则与 cli_homes 一致（USERPROFILE → HOME）；配置里其余字段（api_key 等）
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

/// 枚举本机全部 CLI home（home 根入参化，可单测）：默认 `{root}/.kimi-code` 加
/// glob `{root}/.kimi-code-*`（KIMI_CODE_HOME 可把 CLI home 指到任意路径，托盘读不到
/// CLI 进程的环境变量，只能靠目录发现；拍板：glob 够用，不做设置页手动配目录）。
/// glob 只认横线后缀：`.kimi-code.bak` / `.kimi-code.old` 这类点号命名不匹配，
/// 防备份目录重复计数；默认 home 自身不带横线，不会被 glob 重复匹配。
/// 合法 home 判定：是目录、含 sessions/ 子目录、且含 config.toml 或 credentials/ 之一。
/// 返回顺序确定：默认 home（若合法）在前，其余按路径字典序。
/// pub：statusline 的 tui.toml 写/摘目标与 bin 侧 save_settings 同步都枚举它
/// （跨 crate 访问，不能 pub(crate)）
pub fn cli_homes(home_root: &Path) -> Vec<PathBuf> {
    let mut homes = Vec::new();
    let default_home = home_root.join(".kimi-code");
    if is_valid_cli_home(&default_home) {
        homes.push(default_home);
    }
    if let Ok(entries) = std::fs::read_dir(home_root) {
        let mut extra: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(".kimi-code-"))
                    && is_valid_cli_home(path)
            })
            .collect();
        extra.sort();
        homes.extend(extra);
    }
    homes
}

/// 合法 CLI home 判定：是目录、含 sessions/ 子目录、且含 config.toml 或 credentials/ 之一
/// （缺一视为残骸/半成品目录，跳过防误扫）
fn is_valid_cli_home(dir: &Path) -> bool {
    dir.is_dir()
        && dir.join("sessions").is_dir()
        && (dir.join("config.toml").is_file() || dir.join("credentials").is_dir())
}

/// WSL 侧 CLI home 发现的纯函数（wsl_root 与发行版名单入参化，可单测）：
/// 对每个发行版，枚举 `<wsl_root>/<发行版>/home/` 下每个用户目录的 `.kimi-code`，
/// 外加探测 `<wsl_root>/<发行版>/root/.kimi-code`（root 用户不在 home/ 下）；
/// 合法判定与本地 home 同标准（is_valid_cli_home），结果按路径字典序排序去重。
/// 发行版目录不存在/不可读（WSL 关机、发行版已删）等一切 IO 错误容忍为空
fn wsl_homes_from(wsl_root: &Path, distros: &[String]) -> Vec<PathBuf> {
    let mut homes = Vec::new();
    for distro in distros {
        let distro_root = wsl_root.join(distro);
        if let Ok(entries) = std::fs::read_dir(distro_root.join("home")) {
            for entry in entries.flatten() {
                let candidate = entry.path().join(".kimi-code");
                if is_valid_cli_home(&candidate) {
                    homes.push(candidate);
                }
            }
        }
        let root_home = distro_root.join("root").join(".kimi-code");
        if is_valid_cli_home(&root_home) {
            homes.push(root_home);
        }
    }
    homes.sort();
    homes.dedup();
    homes
}

/// WSL 发行版名单：注册表 HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss 每个
/// 子键（一个已安装发行版一个 GUID 子键）的 DistributionName 值。
/// 背景：\\wsl.localhost 根目录无法枚举（报「UNC 路径格式应为 \\server\share」），
/// 名单只能从这里拿。任何失败（无 WSL、键缺失、读错）返回空 vec，绝不 panic
#[cfg(windows)]
fn wsl_distro_names() -> Vec<String> {
    let Ok(lxss) = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Lxss")
    else {
        return Vec::new();
    };
    lxss.enum_keys()
        .flatten()
        .filter_map(|guid| {
            lxss.open_subkey(guid)
                .and_then(|key| key.get_value::<String, _>("DistributionName"))
                .ok()
        })
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect()
}

/// 非 Windows 无 WSL：恒空（本工具仅发 Windows 版，桩为跨平台编译兜底）
#[cfg(not(windows))]
fn wsl_distro_names() -> Vec<String> {
    Vec::new()
}

/// WSL 侧 CLI home 发现（薄壳）：注册表拿发行版名单 + 以 \\wsl.localhost 为根调纯函数。
/// 环境变量 KIMICODEBAR_WSL_ROOT 可改写根（测试把扫描指向伪造/不存在目录做环境隔离，
/// 生产不设）；注册表名单为空（未装 WSL）时纯函数零迭代，不会触碰 \\wsl.localhost
fn wsl_homes() -> Vec<PathBuf> {
    let root = std::env::var_os("KIMICODEBAR_WSL_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"\\wsl.localhost"));
    wsl_homes_from(&root, &wsl_distro_names())
}

/// 扫描状态路径：{config_dir}/scan-state.json（config_dir 规则与 storage.rs 一致）
fn state_file_path() -> PathBuf {
    crate::storage::config_dir().join("scan-state.json")
}

/// 导出实现（目录/时间入参化以便单测）：写 CSV + 复制历史原文，返回 exports 目录路径。
/// name_suffix 为账号名（多账号每账号一份文件，文件名带后缀区分；None = 无账号兜底）
fn export_report_to<Tz: TimeZone>(
    exports_dir: &Path,
    history_src: &Path,
    points: &[HistoryPoint],
    now: DateTime<Tz>,
    name_suffix: Option<&str>,
) -> Result<PathBuf, String>
where
    Tz::Offset: std::fmt::Display,
{
    let suffix = name_suffix
        .map(sanitize_filename)
        .filter(|s| !s.is_empty())
        .map(|s| format!("-{s}"))
        .unwrap_or_default();
    std::fs::create_dir_all(exports_dir).map_err(|e| format!("创建导出目录失败: {e}"))?;
    let csv_path = exports_dir.join(format!(
        "usage-{}{}.csv",
        now.format("%Y%m%d-%H%M%S"),
        suffix
    ));
    std::fs::write(&csv_path, build_history_csv(points, &now.timezone()))
        .map_err(|e| format!("写入 CSV 失败: {e}"))?;
    // 历史原文一并复制（排查对数用）；源不存在（从未刷新成功过）跳过
    if history_src.exists() {
        std::fs::copy(
            history_src,
            exports_dir.join(format!("history{suffix}.json")),
        )
        .map_err(|e| format!("复制历史原文失败: {e}"))?;
    }
    Ok(exports_dir.to_path_buf())
}

/// 文件名净化：去掉 Windows 文件名非法字符（/\:*?"<>|）与控制字符
fn sanitize_filename(name: &str) -> String {
    name.trim()
        .chars()
        .filter(|c| !r#"/\:*?"<>|"#.contains(*c) && !c.is_control())
        .collect()
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
            last_event_at: None,
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
                },
                crate::storage::Account {
                    id: "acc-b".to_string(),
                    name: "B".to_string(),
                    login_method: Some("oauth".to_string()),
                    provider: "kimi".to_string(),
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
        std::fs::write(
            claude.join("settings.json"),
            format!(r#"{{"env":{{"ANTHROPIC_AUTH_TOKEN":"{token}"}}}}"#),
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
        format!(
            r#"{{"timestamp":"{rfc3339}","type":"turn_context","payload":{{"model":"{model}"}}}}"#
        )
    }

    /// 造 Codex 目录：auth.json（OPENAI_API_KEY）+ sessions/日期分区下 rollout jsonl
    fn write_codex_dir(root: &Path, api_key: &str, lines: &[String]) -> PathBuf {
        let codex = root.join(".codex");
        let day = codex.join("sessions").join("2026").join("08").join("22");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(
            codex.join("auth.json"),
            format!(r#"{{"OPENAI_API_KEY":"{api_key}"}}"#),
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
            r#"{"kimi":{"type":"api","key":"sk-oc-key"},"glm":{"type":"oauth","key":"no"}}"#,
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
        // 旧状态：文件偏移已越过全部内容 + acc-a 桶有既有累计 999
        let legacy = serde_json::json!({
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
        // 落盘的新状态带上了 harness 键（serde(default) 起步）且 buckets 原样
        let saved = std::fs::read_to_string(&state_path).unwrap();
        let state: serde_json::Value = serde_json::from_str(&saved).unwrap();
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
            r#"{"kimi":{"type":"api","key":"sk-oc-data"}}"#,
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
    fn harness_end_to_end_three_harnesses_synthetic() {
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
        // Codex：OPENAI_API_KEY = DeepSeek 账号的 key（跨 provider 归属）
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
            r#"{"kimi":{"type":"api","key":"sk-kimi-e2e"}}"#,
        )
        .unwrap();

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
                },
                crate::storage::Account {
                    id: "acc-ds".to_string(),
                    name: "D".to_string(),
                    login_method: Some("api_key".to_string()),
                    provider: "deepseek".to_string(),
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
        // 环境解析全部指向伪造目录（三家 harness + 配置目录 + 状态目录）
        std::env::set_var("USERPROFILE", &home);
        std::env::set_var("APPDATA", &roaming);
        std::env::set_var("LOCALAPPDATA", &localapp);
        std::env::remove_var("XDG_DATA_HOME");
        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::remove_var("KIMI_SECONDARY_MODEL");
        // 本机真实 WSL home 不进本测试（scan_fresh 会发现它）：根指到不存在目录
        std::env::set_var("KIMICODEBAR_WSL_ROOT", home.join("no-wsl"));

        let view = scan_fresh(now_ms, &chrono::Local);
        // Claude(111) + OpenCode(33) 归 Kimi 账号；Codex(222) 归 DeepSeek 账号
        assert_eq!(view.for_account("acc-kimi").today_tokens, 111 + 33);
        assert_eq!(view.for_account("acc-ds").today_tokens, 222);
        // 三家全部归属成功：未归属桶不生成（账号视图不含未归属数字）
        assert!(!view.by_account.contains_key(UNASSIGNED_BUCKET));
        assert_eq!(view.machine_last_event_at, Some(now_ms));

        // 二次扫描：不重复计数
        let view2 = scan_fresh(now_ms, &chrono::Local);
        assert_eq!(view2.for_account("acc-kimi").today_tokens, 111 + 33);
        assert_eq!(view2.for_account("acc-ds").today_tokens, 222);

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

        // CLI home 1（默认 home）：config.toml 用额外 key k2；一条 wire 事件
        let home_a = home.join(".kimi-code");
        std::fs::create_dir_all(home_a.join("sessions")).unwrap();
        std::fs::write(
            home_a.join("config.toml"),
            format!("[providers.\"managed:kimi-code\"]\napi_key = \"{extra_key}\"\n"),
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
            &format!("[providers.\"managed:kimi-code\"]\napi_key = \"{stray_key}\"\n"),
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

        // WSL home 1（home 用户）：config.toml 用已登记的 key
        let home_a = write_cli_home(
            &wsl_root.join("Ubuntu").join("home"),
            "jyh/.kimi-code",
            "[providers.\"managed:kimi-code\"]\napi_key = \"sk-kimi-wsl-a\"\n",
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
            "[providers.\"managed:kimi-code\"]\napi_key = \"sk-kimi-wsl-unknown\"\n",
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
}
