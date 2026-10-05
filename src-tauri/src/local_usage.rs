//! 本地 Token 消耗统计：增量扫描 Kimi Code 会话的 wire.jsonl 用量事件，
//! 聚合为今日/昨日/最近 7 天/分模型累计（语义移植自 macOS 版 KimiLocalUsage.swift）。
//!
//! 数据源：枚举全部 CLI home（默认 `{userprofile}/.kimi-code` + glob `.kimi-code-*`，
//! 见 cli_homes；另含 WSL 侧 home——发行版名单读注册表 Lxss 键，经 `\\wsl.localhost`
//! 枚举各发行版 home/ 下用户目录与 root/ 的 .kimi-code，见 wsl_homes；再加设置里
//! 手填的远程额外目录 extra_scan_dirs，UNC/Samba 路径，只认 Kimi Code，见 scan_fresh），
//! 递归遍历各 home 的 `sessions/**/wire.jsonl`，逐行 JSON，
//! 只认 `{"type":"usage.record",...}` 事件，实测样例：
//! `{"type":"usage.record","model":"kimi-code/k3","usage":{"inputOther":11592,"output":504,"inputCacheRead":11264,"inputCacheCreation":0},"usageScope":"turn","time":1784973672311}`
//! （time 为 epoch 毫秒；tokens = inputOther + output + inputCacheRead + inputCacheCreation；
//! usageScope 实测恒为 "turn"，不作过滤）
//!
//! 另认嵌套的 `{"type":"context.append_loop_event","event":{"type":"step.end",...,
//! "usage":{...与 usage.record 同形...},"llmStreamDurationMs":2000},"time":...}` 行
//! （实测样例见 parse_step_end_line）：只取 output 与流式时长存为窗口内采样，
//! 聚合「近 10 分钟输出速率 tok/s」（滑动窗口，每次扫描按扫描时刻裁剪；
//! 不进 by_date/by_model 逐日累计，活跃判定 last_event_at 语义也不变）。
//! step.end 的 model 实测常缺：归属链 = 自带 model → 会话最近所见模型
//! （usage.record / 带 model 的 step.end 推进，随 scan-state 的 kimi_models
//! 跨批次记忆）→ CLI 级兜底（attribute_cli）；v1.10.0 直接退兜底会把
//! DeepSeek 模型会话的速率采样张冠李戴到 Kimi 桶（CLI 兜底 Kimi 优先）
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
//!   覆盖 "managed:kimi-code"；"deepseek" 开头 → DeepSeek；其余 provider 下模型名小写
//!   含 "glm" → GLM 路由；dashscope 等第三方 → 未归属）；
//!   查不到按前缀兜底——deepseek 开头 → DeepSeek、glm 开头 → GLM，其余 → Kimi；
//! - Kimi 路由：该 home 的 kimi api_key 与各 Kimi 账号登记的任一 key（主 key 或
//!   任一额外 key，见 creds.rs 的 api_key_extra 槽位）精确相等 → 归该账号；
//!   否则解该 home OAuth access_token（JWT）的 user_id（缺失退 sub）与各账号 OAuth token 的
//!   user_id 比对，相等 → 归该账号；都不中 → 未归属；
//! - DeepSeek 路由：CLI 的 deepseek api_key 与各 DeepSeek 账号登记的任一 key
//!   （主 key 或任一额外 key）精确相等 → 归该账号，否则未归属；
//! - GLM 路由：该 home config.toml [providers] 全部段里凡 api_key 与某 GLM 账号
//!   （Account::is_glm()）主 key 或任一额外 key 精确相等的 key 集合（段名不限、
//!   反向提取；managed:kimi-code / deepseek 两段维持原通道），任一把命中该账号
//!   → 归该账号，否则未归属；GLM 账号与 Kimi 账号同列在 kimi_accounts 里不拆；
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
//! - 已消失文件的偏移按「本轮已扫 root 前缀」清理（见 scan_full）：root 整轮
//!   不可达（停止的 WSL 发行版、探活失败的 UNC 额外目录）≠ 文件删除，其下
//!   既有偏移与模型记忆条目原样保留，恢复可见后从旧偏移续扫、不重复计账；
//!   root 长期不可达时其下真被删的条目随之休眠（不占 CPU 不读盘，无害）；
//! - 缓存命中率：Kimi wire 事件（inputOther+inputCacheRead+inputCacheCreation，
//!   不含 output）与 ZCode 通道（inputTokens 已含缓存读写，见 zcode.rs）按日
//!   另记缓存读与输入总量两条映射；Claude/Codex/OpenCode 事件按 0/0 计入
//!   （不进命中率分子分母）。
//!
//! 跨 Harness 扩展（Claude Code / Codex / OpenCode / ZCode 四家本地日志，解析实现见
//! local_usage/ 子模块）：四家日志与 Kimi home 并列喂同一套分桶聚合器，事件语义
//! 不变（ts 毫秒 / model / tokens），UI 零改动。归属：按各家配置文件里的 API key
//! 与**全部账号（不分 provider）**登记的任一 key（keyring 主 key 或任一额外 key）
//! 精确相等 → 归该账号；
//! 取不到 key 或全不中进未归属桶（OAuth 形态登录设计内落此桶）。扫描状态新增
//! 键（Claude 的 message.id 去重集、Codex 的文件级模型/累计、OpenCode 的
//! time_created 水位 + id 去重集）全走 serde(default)，旧 scan-state 兼容不清零；
//! 去重集按 48 小时裁剪防膨胀。新 harness 事件同样计入机器级活跃判定。
//! ZCode（v1.7.3）零新增状态键：model-io jsonl 逐请求一行 append-only，
//! 去重完全走共用 files 偏移表。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, NaiveDate, TimeZone};
use serde::{Deserialize, Serialize};

use crate::history::HistoryPoint;

mod claude;
mod codex;
mod opencode;
mod zcode;

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
/// 输出速率滑动窗口：只统计扫描时刻近该窗口内的 step.end 采样
const RECENT_RATE_WINDOW_MS: i64 = 10 * 60 * 1000;
/// CSV 表头（与导出约定一致）：时间为本地 ISO（YYYY-MM-DDTHH:mm:ss）
const CSV_HEADER: &str = "time,weekly,five_hour,monthly";

/// 某一天的消耗（与 src/types.ts 的 DailyUsage 一一对应）
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DailyUsage {
    /// 本地日期 YYYY-MM-DD
    pub date: String,
    pub tokens: u64,
    /// 当日缓存命中率（缓存读 / 输入总量，Kimi wire 与 ZCode 通道参与）；
    /// 当日输入总量为 0（无事件 / 纯 output / 其余 harness 事件）为 null
    pub cache_hit_rate: Option<f64>,
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
    /// 近 10 分钟输出速率（tok/s，嵌套 step.end 采样聚合）；窗口内无有效采样为 null
    pub recent_output_tok_per_sec: Option<f64>,
    /// 今日缓存命中率（即 daily 末位那天的 cache_hit_rate，前端免翻 daily）；
    /// 今日输入总量为 0 为 null
    pub today_cache_hit_rate: Option<f64>,
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
/// 另采集四家 harness（Claude Code / Codex / OpenCode / ZCode）的扫描输入一并扫。
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
    // 远程额外扫描目录（设置里手填的 UNC/Samba 路径，issue #57 上半）：只覆盖 Kimi Code
    // 的 sessions/**/wire.jsonl，ZCode/Claude 等 harness 目录仍只认本机、不做远程。
    // 铁规：走独立通道，绝不并进 cli_homes——那函数被 statusline 复用，混进远程目录
    // 会往远程 home 写 tui.toml。
    // 已知风险：UNC 是阻塞 IO 无超时，服务器睡眠/断网会卡住本轮后台扫描
    // （扫描本就跑在 spawn_blocking，不卡 UI，只是本轮结果晚到）。
    for dir in crate::storage::load_settings()
        .unwrap_or_default()
        .extra_scan_dirs
    {
        let home = PathBuf::from(&dir);
        // 先探活：不可达（服务器关机/路径拼错）本轮跳过，warn 不报错（扫描永不失败哲学）；
        // 用 Path::exists（std::fs::exists 要 1.81，本 crate MSRV 1.77），失败同样按 false 跳过
        if !home.exists() {
            tracing::warn!("额外扫描目录不可达，本轮跳过: {dir}");
            continue;
        }
        let attribution = snapshot_attribution(&home);
        targets.push((home.join("sessions"), attribution));
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

/// 四家 harness 的扫描输入（根目录与账号 key 快照，测试可整体伪造绕开环境解析）：
/// - Claude / Codex 只认默认路径（CLAUDE_CONFIG_DIR / CODEX_HOME 指走的 home
///   探测不到，拍板接受）；ZCode 同只认 `{home}/.zcode`；
/// - OpenCode 候选目录存在几个扫几个（按优先级去重）；
/// - key_accounts = 全部账号（不分 provider）的 api_key 快照：harness 事件按
///   key 精确匹配归属，只在内存比对、不落盘
#[derive(Default)]
struct HarnessInput {
    claude_dir: Option<PathBuf>,
    codex_dir: Option<PathBuf>,
    zcode_dir: Option<PathBuf>,
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
        zcode_dir: home.map(|h| h.join(".zcode")),
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
    /// 缓存读分量（缓存命中率分子；Kimi wire 与 ZCode 通道携带，其余 harness 事件恒 0）
    cache_read: u64,
    /// 输入总量（缓存命中率分母，不含 output；Kimi 为 inputOther+inputCacheRead+
    /// inputCacheCreation 之和，ZCode 即 inputTokens——已含缓存读写，见 zcode.rs）
    input_total: u64,
}

/// usage 字段（usage.record 与嵌套 step.end 的 usage 同形）：camelCase 原文映射
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

impl UsageFields {
    /// 全字段求和（usage.record 的 tokens 口径）
    fn total(&self) -> u64 {
        self.input_other + self.output + self.input_cache_read + self.input_cache_creation
    }

    /// 输入总量（缓存命中率分母口径：三个输入分量之和，不含 output）
    fn input_total(&self) -> u64 {
        self.input_other + self.input_cache_read + self.input_cache_creation
    }
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
        tokens: usage.total(),
        cache_read: usage.input_cache_read,
        input_total: usage.input_total(),
    })
}

/// 一条嵌套 step.end 事件的解析结果（输出速率的原料）
#[derive(Debug, PartialEq)]
struct StepEndEvent {
    /// epoch 毫秒（行顶层 time）
    ts_ms: i64,
    /// 嵌套事件可带 model（实测常缺）：带则走模型路由归属；缺则跟随会话最近
    /// 所见模型（扫描循环的 session_model），仍无才退 CLI 级兜底归属
    model: Option<String>,
    output_tokens: u64,
    /// 流式时长（毫秒；解析层已滤 0）
    duration_ms: u64,
}

/// 解析单行 wire.jsonl 里的嵌套 step.end（context.append_loop_event 行 event 键内）：
/// 实测样例 `{"type":"context.append_loop_event","event":{"type":"step.end","turnId":"0",
/// "step":1,"finishReason":"tool_use","usage":{"inputOther":100,"output":10,
/// "inputCacheRead":0,"inputCacheCreation":0},"llmStreamDurationMs":2000},"time":1786600148000}`。
/// llmStreamDurationMs 为 0/缺失的丢弃（速率分母防除零）；usage 缺失按 0 计。
/// 纯函数，可直接单测。
fn parse_step_end_line(line: &str) -> Option<StepEndEvent> {
    #[derive(Deserialize)]
    struct WireLine {
        #[serde(rename = "type")]
        kind: String,
        #[serde(default)]
        time: Option<i64>,
        #[serde(default)]
        event: Option<LoopEvent>,
    }

    #[derive(Deserialize)]
    struct LoopEvent {
        #[serde(rename = "type")]
        kind: String,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        time: Option<i64>,
        #[serde(default)]
        usage: Option<UsageFields>,
        #[serde(rename = "llmStreamDurationMs", default)]
        duration_ms: u64,
    }

    let line: WireLine = serde_json::from_str(line).ok()?;
    if line.kind != "context.append_loop_event" {
        return None;
    }
    let event = line.event?;
    if event.kind != "step.end" {
        return None;
    }
    // 窗口时间取行顶层 time（实测在此层）；个别形态带在嵌套事件里时兜底
    let ts_ms = line.time.or(event.time)?;
    if event.duration_ms == 0 {
        return None;
    }
    Some(StepEndEvent {
        ts_ms,
        model: event.model,
        output_tokens: event.usage.map_or(0, |u| u.output),
        duration_ms: event.duration_ms,
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

/// step.end 采样（近 10 分钟输出速率的原料，随聚合器落盘、每次扫描按窗口裁剪）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct StepSample {
    /// epoch 毫秒
    ts_ms: i64,
    /// 该步 output tokens
    output: u64,
    /// 流式时长（毫秒，恒 > 0：解析层已滤 0）
    duration_ms: u64,
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
    /// 本地日期（YYYY-MM-DD）→ 缓存读 tokens（缓存命中率分子；Kimi wire 与
    /// ZCode 通道携带分量，其余 harness 事件恒加 0）
    #[serde(default)]
    by_date_cache_read: HashMap<String, u64>,
    /// 本地日期（YYYY-MM-DD）→ 输入总量 tokens（缓存命中率分母，不含 output）
    #[serde(default)]
    by_date_input: HashMap<String, u64>,
    /// 近 10 分钟滑动窗口内的 step.end 采样（输出速率原料；每次扫描裁剪防膨胀）
    #[serde(default)]
    step_samples: Vec<StepSample>,
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
            let models = self.by_date_model.entry(date.clone()).or_default();
            *models.entry(event.model.clone()).or_insert(0) += event.tokens;
            // 命中率分子分母：Kimi/ZCode 携带真实分量，Claude/Codex/OpenCode
            // 为 0/0 不影响比率（全 0/0 日分母为 0 → None）
            *self.by_date_cache_read.entry(date.clone()).or_insert(0) += event.cache_read;
            *self.by_date_input.entry(date).or_insert(0) += event.input_total;
        }
    }

    /// 喂入一条 step.end 采样（零时长已在解析层滤除，不进 here）
    fn add_step(&mut self, sample: StepSample) {
        self.step_samples.push(sample);
    }

    /// 裁剪 10 分钟窗口外的采样（滑动窗口：每次扫描以当前时刻为锚，控制状态体积）
    fn prune_steps(&mut self, now_ms: i64) {
        let cutoff = now_ms.saturating_sub(RECENT_RATE_WINDOW_MS);
        self.step_samples.retain(|s| s.ts_ms >= cutoff);
    }

    /// 近 10 分钟输出速率（tok/s）：窗口内采样 output 总和 ÷ duration 总和 × 1000。
    /// 窗口内无有效采样（零时长事件解析层即丢弃）或总时长为 0 → None
    fn recent_output_rate(&self, now_ms: i64) -> Option<f64> {
        let cutoff = now_ms.saturating_sub(RECENT_RATE_WINDOW_MS);
        let (tokens, duration) = self
            .step_samples
            .iter()
            .filter(|s| s.ts_ms >= cutoff)
            .fold((0u64, 0u64), |(t, d), s| (t + s.output, d + s.duration_ms));
        if duration == 0 {
            return None;
        }
        Some(tokens as f64 / duration as f64 * 1000.0)
    }

    /// 丢弃 30 天前的按日聚合（四条映射同一窗口），控制状态文件体积
    fn prune(&mut self, today: NaiveDate) {
        let cutoff = (today - chrono::Duration::days(BY_DATE_RETENTION_DAYS))
            .format("%Y-%m-%d")
            .to_string();
        // 日期键零填充定长，字符串序即日期序
        self.by_date.retain(|date, _| date >= &cutoff);
        self.by_date_model.retain(|date, _| date >= &cutoff);
        self.by_date_cache_read.retain(|date, _| date >= &cutoff);
        self.by_date_input.retain(|date, _| date >= &cutoff);
    }

    /// 由累计聚合出统计视图：today 为本地今天；now_ms 为扫描时刻（输出速率窗口锚点）；
    /// daily 为最近 7 个自然日（升序、缺日补 0）；by_model 为今日分模型降序 top 5
    fn finish(&self, today: NaiveDate, last_scan_at: Option<i64>, now_ms: i64) -> LocalUsageStats {
        let day_tokens = |date: NaiveDate| {
            self.by_date
                .get(&date.format("%Y-%m-%d").to_string())
                .copied()
                .unwrap_or(0)
        };
        // 当日缓存命中率 = 缓存读 / 输入总量；输入为 0（无事件 / 纯 output /
        // 全 0/0 harness 事件）→ None（前端不渲染该行）
        let day_cache_hit_rate = |date: NaiveDate| {
            let key = date.format("%Y-%m-%d").to_string();
            let input = self.by_date_input.get(&key).copied().unwrap_or(0);
            if input == 0 {
                return None;
            }
            Some(self.by_date_cache_read.get(&key).copied().unwrap_or(0) as f64 / input as f64)
        };
        let daily = (0..DAILY_DAYS)
            .rev()
            .map(|i| {
                let date = today - chrono::Duration::days(i);
                DailyUsage {
                    date: date.format("%Y-%m-%d").to_string(),
                    tokens: day_tokens(date),
                    cache_hit_rate: day_cache_hit_rate(date),
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
            recent_output_tok_per_sec: self.recent_output_rate(now_ms),
            today_cache_hit_rate: day_cache_hit_rate(today),
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

/// scan-state.json 的格式版本：归属/聚合规则发生变化即 +1，load_state 发现不一致
/// 整体丢弃全量重扫（老用户的历史消耗按新规则重新归属）。
/// 1 = 分账号 buckets 时代（无 version 字段的隐式版本）；2 = 新增 GLM 归属路由；
/// 3 = 新增近 10 分钟 step.end 输出速率聚合（UsageAggregator 新增 step_samples）；
/// 4 = 缺 model 的 step.end 改随会话级模型记忆（kimi_models）归属——修 DeepSeek
///     模型会话的速率采样被 CLI 兜底（Kimi 优先）张冠李戴到 Kimi 桶；
/// 5 = WSL 抖动重复计账根修（四张表的 retain 清理从「全盘 disk_paths」改为
///     「按本轮已扫 root 前缀」，见 scan_full）+ 按日缓存命中率聚合
///     （by_date_cache_read / by_date_input）。存量虚高账本随版本不一致整体
///     丢弃、按现存文件全量重扫归位（拍板：立刻清净，不等 30 天自然衰减；
///     已被删除会话的历史消耗随之消失，诚实口径）；
/// 6 = ZCode（GLM）通道补缓存分量（cache_read = cacheReadTokens、input_total =
///     inputTokens，见 zcode.rs）+ tokens 口径修正（inputTokens 已含缓存读写，
///     原四项相加把缓存读/写加了第二遍、虚高约 1.9 倍）。同为升级即全量重扫：
///     已被 ZCode 轮转删除的旧日志随之出账（拍板接受，账面归真实）
const STATE_VERSION: u32 = 6;

/// 扫描状态（scan-state.json）：格式版本 + 文件偏移 + 分桶累计聚合。损坏/不存在容忍为空状态重新全扫
#[derive(Debug, Default, Serialize, Deserialize)]
struct ScanState {
    /// 格式版本（缺失按 0 = GLM 归属路由之前的旧版）：与 STATE_VERSION 不一致即整体丢弃
    #[serde(default)]
    version: u32,
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
    /// Kimi wire.jsonl 文件路径 → 会话最近所见模型（usage.record / 带 model 的
    /// step.end 推进；增量续扫下跨批次记忆，供缺 model 的 step.end 归属用）
    #[serde(default)]
    kimi_models: HashMap<String, String>,
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
    /// GLM 模型（[models] 表命中且 provider 无 kimi/deepseek 标记、模型名小写含 "glm"；
    /// 或查不到表时前缀兜底 glm 开头）：走 config.toml [providers] 各段的 GLM key 反向比对
    Glm,
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
    /// config.toml [providers] 全部段里与某 GLM 账号（is_glm()）主 key 或任一额外 key
    /// 精确相等的 api_key 集合（段名不限、反向提取；managed:kimi-code / deepseek 两段
    /// 维持各自原通道，Kimi/DeepSeek 路由不读本字段）
    glm_api_keys: Vec<String>,
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
    /// (账号 id, 凭证)，Kimi 与 GLM 账号（GLM 账号同在此列表，statusline 解析依赖此结构）
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
/// "deepseek" 开头 → DeepSeek，其余 provider 下模型名小写含 "glm" → GLM 路由，
/// dashscope 等第三方 → 未归属；查不到按前缀兜底——deepseek 开头 → DeepSeek、
/// glm 开头 → GLM，其余 → Kimi
fn route_model(model: &str, attribution: &Attribution) -> Route {
    match attribution.model_providers.get(model) {
        Some(provider) if provider.contains("kimi") => Route::Kimi,
        Some(provider) if provider.starts_with("deepseek") => Route::DeepSeek,
        Some(_) if model.to_lowercase().contains("glm") => Route::Glm,
        Some(_) => Route::Unassigned,
        None if model.starts_with("deepseek") => Route::DeepSeek,
        None if model.to_lowercase().starts_with("glm") => Route::Glm,
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
        Route::Glm => {
            // GLM 账号与 Kimi 账号同列在 kimi_accounts（不拆列表，statusline 依赖）；
            // 任一把 GLM key 命中其主/额外 key 即归该账号
            if let Some((id, _)) = attribution.cli.glm_api_keys.iter().find_map(|key| {
                attribution
                    .kimi_accounts
                    .iter()
                    .find(|(_, creds)| account_has_key(creds, key))
            }) {
                return id.clone();
            }
            UNASSIGNED_BUCKET.to_string()
        }
        Route::Unassigned => UNASSIGNED_BUCKET.to_string(),
    }
}

/// statusline 专用归属（pub(crate)：statusline.rs 的账号解析用它）：
/// statusline 进程没有事件模型可路由，直接按 CLI 侧凭证通道判定——
/// 优先 Kimi 通道（api_key 比对 → OAuth user_id 比对；GLM key 也在
/// managed:kimi-code 槽位，同走此通道）；未命中再走 DeepSeek 通道兜底
/// （一个 home 双 provider 的场景）；两者都不中且快照含 GLM key 时走
/// GLM 兜底（用 "glm" 作模型名触发 Glm 路由）；全不中返回未归属桶键
pub(crate) fn attribute_cli(attribution: &Attribution) -> String {
    let kimi_bucket = attribute("managed:kimi-code", attribution);
    if kimi_bucket != UNASSIGNED_BUCKET {
        return kimi_bucket;
    }
    if attribution.cli.deepseek_api_key.is_some() {
        let deepseek_bucket = attribute("deepseek", attribution);
        if deepseek_bucket != UNASSIGNED_BUCKET {
            return deepseek_bucket;
        }
    }
    if !attribution.cli.glm_api_keys.is_empty() {
        let glm_bucket = attribute("glm", attribution);
        if glm_bucket != UNASSIGNED_BUCKET {
            return glm_bucket;
        }
    }
    UNASSIGNED_BUCKET.to_string()
}

/// 归属上下文快照：指定 CLI home 的凭证（该 home 的 config.toml 的 kimi/deepseek
/// api_key、[providers] 各段里命中 GLM 账号的 key 集合与 [models] 路由表、
/// credentials/kimi-code.json 的 OAuth user_id）+ 各账号凭证
/// （keyring 主 key + 额外 key / OAuth user_id，与 home 无关，逐 home 快照时每次重读）。
/// 所有读取失败（文件缺失/损坏、keyring 错误、凭证未配置）一律容忍为空——扫描永不失败；
/// 某 home 快照为空时该 home 的事件全部进未归属桶，机器级活跃判定不受影响。
/// pub(crate)：statusline 的账号解析（statusline.rs）要复用同一份快照
pub(crate) fn snapshot_attribution(home: &Path) -> Attribution {
    let mut attribution = Attribution::default();
    // [providers] 全部段的 api_key（GLM 反向提取用；段名不限，见下）
    let mut provider_keys: Vec<String> = Vec::new();
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
            if let Some(providers) = doc.get("providers").and_then(|p| p.as_table()) {
                for def in providers.values() {
                    if let Some(key) = def
                        .get("api_key")
                        .and_then(|k| k.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    {
                        provider_keys.push(key.to_string());
                    }
                }
            }
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
    // GLM 账号的主/额外 key 集合：账号列表先加载（上一条循环），再反向匹配
    // [providers] 各段（段名不限，用户自定义）——命中即记入 glm_api_keys，
    // managed:kimi-code / deepseek 两段的 key 也在此集合，但只影响 Glm 路由
    let mut glm_account_keys: Vec<String> = Vec::new();
    for account in &crate::storage::load_settings().unwrap_or_default().accounts {
        let api_key = crate::creds::load_api_key(&account.id)
            .ok()
            .flatten()
            .map(|k| k.trim().to_string())
            .filter(|s| !s.is_empty());
        // 额外 key（trim + 滤空）：读取失败容忍为空数组，与主 key 同权参与归属
        let extra_api_keys: Vec<String> = crate::creds::load_api_key_extra(&account.id)
            .unwrap_or_default()
            .into_iter()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .collect();
        if account.is_glm() {
            glm_account_keys.extend(api_key.iter().cloned());
            glm_account_keys.extend(extra_api_keys.iter().cloned());
        }
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
    // 账号列表已加载完毕：把 [providers] 各段里命中 GLM 账号任一登记的 key 记入快照
    // （去重保序；比对只认精确相等）
    for key in provider_keys {
        if glm_account_keys.contains(&key) && !attribution.cli.glm_api_keys.contains(&key) {
            attribution.cli.glm_api_keys.push(key);
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
        aggregator.prune_steps(now_ms);
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

    // ---- 四家 harness：文件发现与归属 key 快照（扫描开头一次）----
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
    let zcode_keys = harness
        .zcode_dir
        .as_deref()
        .map(zcode::auth_keys)
        .unwrap_or_default();
    let codex_files = harness
        .codex_dir
        .as_deref()
        .map(codex::collect_files)
        .unwrap_or_default();
    let zcode_files = harness
        .zcode_dir
        .as_deref()
        .map(zcode::collect_files)
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
    // （Kimi wire.jsonl 与 Claude/Codex jsonl 的偏移同住一张表，清理统一做）。
    // 清理范围限定「本轮已扫 root 前缀」：本轮进入扫描目标的 root（scan_targets
    // 各 sessions 目录 + harness 的 claude/codex/zcode 目录，无论是否扫到文件）
    // 才清理其前缀下缺席的路径；root 整轮没进目标 ≠ 文件删除——停止的 WSL
    // 发行版被 wsl_homes 过滤、exists() 探活失败的 extra_scan_dirs UNC 目录，
    // 其前缀下既有偏移与各模型记忆条目原样保留，恢复可见后从旧偏移续扫、
    // 不会全量重读重复计账（实锤过：WSL 抖动使 30 天窗口历史被整轮重计）。
    // 前缀匹配按路径分量（Path::starts_with 语义），禁止裸字符串前缀
    // （防 /tmp/x 误清 /tmp/x2）；状态键与扫描目标同源枚举（同一批
    // to_string_lossy 产物），Windows 路径大小写一致性由此保证。
    // 已知取舍（拍板接受）：root 长期不可达时其下真被删的文件条目休眠不清理
    // （不占 CPU 不读盘，体积可忽略；会话文件 uuid 命名，同名撞路径实际不可能）；
    // WSL 发行版整体卸载会留死条目，同样无害。
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
    disk_paths.extend(zcode_files.iter().map(|f| f.to_string_lossy().into_owned()));
    let scanned_roots: Vec<&Path> = scan_targets
        .iter()
        .map(|(dir, _)| dir.as_path())
        .chain(harness.claude_dir.as_deref())
        .chain(harness.codex_dir.as_deref())
        .chain(harness.zcode_dir.as_deref())
        .collect();
    let under_scanned_root = |path: &str| {
        let path = Path::new(path);
        scanned_roots.iter().any(|root| path.starts_with(root))
    };
    state
        .files
        .retain(|p, _| disk_paths.contains(p) || !under_scanned_root(p));
    state
        .codex_models
        .retain(|p, _| disk_paths.contains(p) || !under_scanned_root(p));
    state
        .codex_totals
        .retain(|p, _| disk_paths.contains(p) || !under_scanned_root(p));
    state
        .kimi_models
        .retain(|p, _| disk_paths.contains(p) || !under_scanned_root(p));

    for (path, attribution) in &files {
        let key = path.to_string_lossy().into_owned();
        let offset = state.files.get(&key).copied().unwrap_or(0);
        match read_new_lines(path, offset) {
            Ok((lines, new_offset)) => {
                // 会话级模型记忆（随 scan-state 落盘，增量续扫跨批次保留）：
                // usage.record / 带 model 的 step.end 推进它，供缺 model 的
                // step.end 跟随会话实际模型归属（修 v1.10.0 直接退 CLI 兜底把
                // DeepSeek 会话的速率采样张冠李戴到 Kimi 桶）
                let mut session_model = state.kimi_models.get(&key).cloned();
                for line in &lines {
                    if let Some(event) = parse_usage_line(line) {
                        // "unknown" 是缺 model 的占位桶，不是真实模型，不做记忆
                        if event.model != "unknown" {
                            session_model = Some(event.model.clone());
                        }
                        let bucket = attribute(&event.model, attribution);
                        state.buckets.entry(bucket).or_default().add(&event, tz);
                    } else if let Some(step) = parse_step_end_line(line) {
                        if let Some(model) = &step.model {
                            session_model = Some(model.clone());
                        }
                        // 归属链：step 自带 model → 会话最近所见模型 → CLI 级兜底
                        let bucket = match step.model.as_deref().or(session_model.as_deref()) {
                            Some(model) => attribute(model, attribution),
                            None => attribute_cli(attribution),
                        };
                        state
                            .buckets
                            .entry(bucket)
                            .or_default()
                            .add_step(StepSample {
                                ts_ms: step.ts_ms,
                                output: step.output_tokens,
                                duration_ms: step.duration_ms,
                            });
                    }
                }
                if let Some(model) = session_model {
                    state.kimi_models.insert(key.clone(), model);
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

    // ZCode：续读 → 逐行直接出账（model-io 逐请求一行 append-only，无流式重写，
    // 无需 Claude 式去重也无 Codex 式累计基线；字节偏移即全部状态）。
    // 单配置一把 key：v2/config.json 全 provider 候选任一命中账号 key 即归，
    // 扫描开头判一次，整批共用
    let zcode_key = zcode_keys
        .iter()
        .find(|key| {
            harness
                .key_accounts
                .iter()
                .any(|(account_key, _)| account_key == *key)
        })
        .cloned();
    for path in &zcode_files {
        let key = path.to_string_lossy().into_owned();
        let offset = state.files.get(&key).copied().unwrap_or(0);
        if let Ok((lines, new_offset)) = read_new_lines(path, offset) {
            for event in lines.iter().filter_map(|line| zcode::parse_line(line)) {
                harness_events.push((event, zcode_key.clone()));
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
        .map(|(key, agg)| (key.clone(), agg.finish(today, state.last_scan_at, now_ms)))
        .collect();
    ScanView {
        machine_last_event_at,
        by_account,
        last_scan_at: state.last_scan_at,
        // 空聚合器出 7 天零值模板：无桶账号页显示诚实零（daily 逐日连续契约不破）
        empty: UsageAggregator::default().finish(today, state.last_scan_at, now_ms),
    }
}

/// 空状态（等价首次全扫）：带当前格式版本，save_state 落盘后下次 load 不再被判为旧版
fn fresh_state() -> ScanState {
    ScanState {
        version: STATE_VERSION,
        ..Default::default()
    }
}

/// 读扫描状态：文件不存在/损坏 → 空状态（等价首次全扫）。
/// 旧版状态（机器级 totals 合计、无 buckets 键；或 version 与 STATE_VERSION 不一致）
/// 整体丢弃：返回空状态即「清空聚合 + 全部文件偏移归零」，本次扫描全量重读重建分桶
/// （拍板：旧合计不做任何保留；版本不一致 = 归属规则已变，历史按新规则重新归属）
fn load_state(path: &Path) -> ScanState {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => return fresh_state(),
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return fresh_state();
    };
    if value.get("buckets").is_none() {
        return fresh_state();
    }
    match serde_json::from_value::<ScanState>(value) {
        Ok(state) if state.version == STATE_VERSION => state,
        _ => fresh_state(),
    }
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

/// 正在运行的 WSL 发行版名单：`wsl.exe -l --running -q`（只查询，不拉起任何停止的
/// 发行版——而访问停止发行版的 \\wsl.localhost\<distro> 路径会把 VM 拉起来，
/// 轮询每 5 分钟拉起一次 = 全屏游戏被踢/焦点丢失的疑似真凶）。任何失败（无 wsl.exe、
/// 非零退出）返回 None，调用方据此 fail-closed 本轮停扫（绝不冒拉起 VM 的风险）。
#[cfg(windows)]
fn wsl_running_distros() -> Option<Vec<String>> {
    use std::os::windows::process::CommandExt;
    // CREATE_NO_WINDOW：本进程是无控制台的 GUI 程序，防派生控制台窗口闪现
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("wsl.exe")
        .args(["-l", "--running", "-q"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_wsl_distro_list(&out.stdout))
}

/// 非 Windows 无 WSL：恒 None（桩为跨平台编译兜底）
#[cfg(not(windows))]
fn wsl_running_distros() -> Option<Vec<String>> {
    None
}

/// 解析 `wsl -l -q` 输出（纯函数，可单测）：现代 WSL 输出 UTF-16LE（可带可不带
/// BOM，特征是近半字节为 NUL），旧版 UTF-8；按行拆分、去空白、丢空行。
fn parse_wsl_distro_list(raw: &[u8]) -> Vec<String> {
    let sample = &raw[..raw.len().min(64)];
    let nul_ratio = sample.iter().filter(|&&b| b == 0).count() as f64 / sample.len().max(1) as f64;
    let text = if raw.starts_with(&[0xFF, 0xFE]) || nul_ratio > 0.2 {
        let stripped = raw.strip_prefix(&[0xFF, 0xFE]).unwrap_or(raw);
        let units: Vec<u16> = stripped
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(raw).into_owned()
    };
    text.lines()
        .map(|l| l.trim_matches('\0').trim())
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// 已注册 ∩ 正在运行的发行版交集（纯函数，可单测）：大小写不敏感
/// （注册表名与 wsl -l 输出的大小写在不同版本间漂移过）
fn filter_running_distros(registered: Vec<String>, running: &[String]) -> Vec<String> {
    registered
        .into_iter()
        .filter(|d| running.iter().any(|r| r.eq_ignore_ascii_case(d)))
        .collect()
}

/// WSL 侧 CLI home 发现（薄壳）：注册表拿发行版名单 + 以 \\wsl.localhost 为根调纯函数。
/// **只扫正在运行的发行版**：访问停止发行版的 UNC 路径会把 WSL 虚拟机拉起来
/// （2026-09-08 实机确认本机 Ubuntu 常年 Stopped，每次轮询扫描 = 一次 VM 冷启动）。
/// wsl.exe 查询失败（None）时本轮停扫。环境变量 KIMICODEBAR_WSL_ROOT 可改写根
/// （测试把扫描指向伪造/不存在目录做环境隔离，生产不设），改写时跳过运行态过滤——
/// 测试根本来就不碰真实 UNC。
fn wsl_homes() -> Vec<PathBuf> {
    if let Some(root) = std::env::var_os("KIMICODEBAR_WSL_ROOT").map(PathBuf::from) {
        return wsl_homes_from(&root, &wsl_distro_names());
    }
    let Some(running) = wsl_running_distros() else {
        return Vec::new();
    };
    let registered = wsl_distro_names();
    let targets = filter_running_distros(registered.clone(), &running);
    if targets.is_empty() && !registered.is_empty() {
        tracing::info!("WSL 发行版均未在运行，本轮跳过 WSL home 扫描（不拉起停止的 VM）");
    }
    wsl_homes_from(Path::new(r"\\wsl.localhost"), &targets)
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
mod tests;
