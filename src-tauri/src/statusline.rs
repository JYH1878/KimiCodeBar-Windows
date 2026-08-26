//! Kimi Code 状态栏（`tui.toml [status_line].command`）的后端实现。
//!
//! Kimi Code CLI 每次刷新状态栏（300ms 上限、1/s 节流）执行该命令，
//! stdout 首行显示在 TUI 底栏；命令失败/非零退出时回退内置布局。
//! 因此 `kimicodebar.exe --statusline` 必须是纯本地、零网络、同步快路径：
//! 只读设置 + 缓存渲染一行即退，绝不建 tokio runtime。
//!
//! 三块职责：
//! - `render_line`：按 provider 把 `storage::CachedQuota` 渲染成一行（纯函数，可单测）；
//! - `cli_home` / `resolve_account`：由 CLI 派生的进程继承其环境变量——`KIMI_CODE_HOME`
//!   有值用它、否则默认 home，对该 home 做凭证归属（复用 local_usage 的归属件），
//!   归属不中回退第一个账号；
//! - `install` / `uninstall`：设置页开关保存时写/摘 tui.toml 的 command——幂等、
//!   绝不覆盖用户自定义命令、其他字段（含 status_line.items）原样保留。

use std::path::{Path, PathBuf};

use crate::i18n::Lang;
use crate::local_usage;
use crate::storage::{Account, CachedQuota, Settings};

/// tui.toml command 里的我们的标记：含它即视为我们安装的命令（摘除/幂等判定用）
const MARKER: &str = "--statusline";
/// 缓存距今超过该秒数（10 分钟）视为过期，尾部追加 " · N分钟前"
const STALE_AFTER_SECS: i64 = 600;

// ---------------------------------------------------------------------------
// 格式化：纯函数，全部可单测
// ---------------------------------------------------------------------------

/// 渲染状态栏单行文本（纯函数）：
/// - Kimi / GLM（GLM 额度本就映射进 KimiQuota）：`Kimi 5小时 42% · 7天 18% · Allegro`，
///   五小时段在前、7 天段在后，membership_level 有值才带档位段，缺的段跳过；
///   任一已展示窗口的 percent_remaining（剩余语义）严格低于 warn_threshold_pct 时
///   整行前缀 `⚠ `；
/// - DeepSeek：`DeepSeek 余额 ¥123.45`（金额规则复用 i18n），is_available=false 追加
///   ` · 不可用`；
/// - 缓存 fetched_at 距 now_secs 超 10 分钟时尾部追加 ` · N分钟前`（N≥60 显示 `N小时前`）。
///
/// 没有任何可展示内容时返回空串（调用方按"无可展示"非零退出，让 CLI 回退内置布局）
pub fn render_line(
    account: &Account,
    cache: &CachedQuota,
    lang: Lang,
    warn_threshold_pct: f64,
    now_secs: i64,
) -> String {
    let mut line = if account.is_deepseek() {
        render_deepseek(cache, lang)
    } else {
        render_kimi(cache, lang, warn_threshold_pct)
    };
    append_stale_suffix(&mut line, lang, cache.fetched_at, now_secs);
    line
}

/// Kimi / GLM 行：`[⚠ ]Kimi 5小时 42% · 7天 18%[ · 档位]`；两窗与档位全缺 → 空串
fn render_kimi(cache: &CachedQuota, lang: Lang, warn_threshold_pct: f64) -> String {
    let quota = &cache.quota;
    // 任一已展示窗口剩余百分比低于阈值即整行告警（严格小于，与 needs_low_warning 同语义）
    let low = quota
        .five_hour
        .as_ref()
        .is_some_and(|f| f.percent_remaining < warn_threshold_pct)
        || quota
            .weekly
            .as_ref()
            .is_some_and(|w| w.percent_remaining < warn_threshold_pct);
    let mut parts: Vec<String> = Vec::new();
    if let Some(five_hour) = &quota.five_hour {
        parts.push(match lang {
            Lang::Zh => format!("5小时 {:.0}%", five_hour.percent_remaining),
            Lang::En => format!("5h {:.0}%", five_hour.percent_remaining),
        });
    }
    if let Some(weekly) = &quota.weekly {
        parts.push(match lang {
            Lang::Zh => format!("7天 {:.0}%", weekly.percent_remaining),
            Lang::En => format!("7d {:.0}%", weekly.percent_remaining),
        });
    }
    if let Some(level) = quota.membership_level.as_deref().filter(|s| !s.is_empty()) {
        parts.push(membership_display(level).to_string());
    }
    if parts.is_empty() {
        return String::new();
    }
    let prefix = if low { "⚠ " } else { "" };
    format!("{prefix}Kimi {}", parts.join(" · "))
}

/// 会员等级枚举 → 官方档位名（与面板 MembershipCard 同款映射；未知等级原样显示）
fn membership_display(level: &str) -> &str {
    match level {
        "LEVEL_FREE" => "Andante",
        "LEVEL_BASIC" => "Moderato",
        "LEVEL_INTERMEDIATE" => "Allegretto",
        "LEVEL_ADVANCED" => "Allegro",
        other => other,
    }
}

/// DeepSeek 行：`DeepSeek 余额 ¥123.45`（文案与金额规则复用 i18n::deepseek_summary），
/// is_available=false 追加 ` · 不可用`；缓存无余额（旧格式缓存）→ 空串
fn render_deepseek(cache: &CachedQuota, lang: Lang) -> String {
    let Some(balance) = &cache.deepseek_balance else {
        return String::new();
    };
    let mut line = crate::i18n::deepseek_summary(lang, balance);
    if !balance.is_available {
        line.push_str(match lang {
            Lang::Zh => " · 不可用",
            Lang::En => " · unavailable",
        });
    }
    line
}

/// 新鲜度后缀（纯函数）：fetched_at 距 now_secs 严格超过 10 分钟才追加；
/// N = 分钟数取整除，N≥60 时以小时计（如 90 分钟 → "1小时前"）；时钟回拨无后缀
fn append_stale_suffix(line: &mut String, lang: Lang, fetched_at: i64, now_secs: i64) {
    let age_secs = now_secs - fetched_at;
    if age_secs <= STALE_AFTER_SECS {
        return;
    }
    let minutes = age_secs / 60;
    if minutes >= 60 {
        let hours = minutes / 60;
        line.push_str(&match lang {
            Lang::Zh => format!(" · {hours}小时前"),
            Lang::En => format!(" · {hours}h ago"),
        });
    } else {
        line.push_str(&match lang {
            Lang::Zh => format!(" · {minutes}分钟前"),
            Lang::En => format!(" · {minutes}m ago"),
        });
    }
}

// ---------------------------------------------------------------------------
// stdin 快照：自定义命令的输出整体替换第一行底栏（模型/工作区本在那行），
// 把 Kimi Code 传入的快照关键信息拼回输出行，额度与原有信息共存
// ---------------------------------------------------------------------------

/// Kimi Code 经 stdin 传入的快照（0.38.0 实机抓取的字段形态）：
/// `{"model","cwd","gitBranch","permissionMode","planMode","contextUsage",
///   "contextTokens","maxContextTokens","sessionId","version"}`
/// 思考力度与 swarm 开关不在快照里，无法还原；上下文用量在第二行底栏，不受影响
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Snapshot {
    pub model: Option<String>,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub permission_mode: Option<String>,
    pub plan_mode: bool,
}

/// 从 stdin 文本解析快照（纯函数）：空串/损坏/非对象 → None（调用方退化为纯额度行）；
/// model 与 cwd 全缺也视为无快照（避免拼出只有分隔符的怪行）
pub fn parse_snapshot(stdin_text: &str) -> Option<Snapshot> {
    let v: serde_json::Value = serde_json::from_str(stdin_text.trim()).ok()?;
    let obj = v.as_object()?;
    let get = |key: &str| {
        obj.get(key)
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let snapshot = Snapshot {
        model: get("model"),
        cwd: get("cwd"),
        git_branch: get("gitBranch"),
        permission_mode: get("permissionMode"),
        plan_mode: obj
            .get("planMode")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
    };
    if snapshot.model.is_none() && snapshot.cwd.is_none() {
        return None;
    }
    Some(snapshot)
}

/// 渲染快照前缀（纯函数）：`K3-256k · C:\Users\JYH\proj ⎇ main · yolo · plan`。
/// manual 权限模式是默认态，不显示以降低噪音；全空 → 空串
pub fn render_snapshot_prefix(snapshot: &Snapshot) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(model) = &snapshot.model {
        parts.push(model.clone());
    }
    let cwd_part = match (&snapshot.cwd, &snapshot.git_branch) {
        (Some(cwd), Some(branch)) => format!("{cwd} ⎇ {branch}"),
        (Some(cwd), None) => cwd.clone(),
        (None, Some(branch)) => format!("⎇ {branch}"),
        (None, None) => String::new(),
    };
    if !cwd_part.is_empty() {
        parts.push(cwd_part);
    }
    if let Some(mode) = snapshot
        .permission_mode
        .as_deref()
        .filter(|m| *m != "manual")
    {
        parts.push(mode.to_string());
    }
    if snapshot.plan_mode {
        parts.push("plan".to_string());
    }
    parts.join(" · ")
}

/// 复合行（纯函数）：`快照前缀 │ 额度行`；额度行为空 → 仅快照前缀（底栏仍有信息量）；
/// 无快照 → 仅额度行（人工运行 / 旧版 CLI 场景）
pub fn compose_line(snapshot: Option<&Snapshot>, quota_line: String) -> String {
    let prefix = snapshot.map(render_snapshot_prefix).unwrap_or_default();
    match (prefix.is_empty(), quota_line.is_empty()) {
        (false, false) => format!("{prefix} │ {quota_line}"),
        (false, true) => prefix,
        (true, _) => quota_line,
    }
}

// ---------------------------------------------------------------------------
// 账号解析：CLI 派生的进程继承其环境变量，按归属选展示账号
// ---------------------------------------------------------------------------

/// statusline 进程的 CLI home：`KIMI_CODE_HOME`（非空）优先——CLI 派生我们时
/// 必然继承它；否则回退 `{USERPROFILE|HOME}/.kimi-code`
pub fn cli_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("KIMI_CODE_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    let root = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(root).join(".kimi-code"))
}

/// 选定展示账号（纯本地、零网络，300ms 预算内）：
/// 1. 按 CLI home 做凭证归属（snapshot_attribution 同步直读 keyring，不建 runtime），
///    命中且该账号有缓存 → 归属账号；
/// 2. 否则（归属不中 / 归属账号无缓存）→ 第一个账号（调用方再判它有无缓存）；
/// 3. 一个账号都没有 → None（调用方无输出非零退出）
pub fn resolve_account(settings: &Settings) -> Option<Account> {
    let first = settings.accounts.first()?;
    if let Some(home) = cli_home() {
        let attribution = local_usage::snapshot_attribution(&home);
        let bucket = local_usage::attribute_cli(&attribution);
        if bucket != local_usage::UNASSIGNED_BUCKET {
            if let Some(account) = settings.account(&bucket) {
                if crate::storage::load_cache(&account.id).is_some() {
                    return Some(account.clone());
                }
            }
        }
    }
    Some(first.clone())
}

// ---------------------------------------------------------------------------
// tui.toml 写 / 摘：幂等、绝不覆盖用户自定义命令、其他字段原样保留
// ---------------------------------------------------------------------------

/// 安装状态栏命令：把 `[status_line].command` 设为 `<current_exe> --statusline`
/// （exe 路径取 std::env::current_exe()，toml 序列化自动处理反斜杠转义）。
/// - **路径不加引号**：Kimi Code 执行 status_line.command 不走 shell、按空格切分 argv
///   （2026-08-25 实机抓到：带引号时 argv[0] 变 `"D:\..."` 字面量直接 ENOENT，
///   状态栏静默回退内置布局）；因此含空格的路径同样无法寻址，直接报错不装；
/// - 幂等：现有 command 与期望形态一致 → 直接跳过（不重写文件）；含我们的标记但
///   形态过时（如早期带引号形态）→ 重写到最新形态；
/// - 用户已有非我们的自定义 command → 报冲突错误，绝不覆盖；
/// - 其他字段（含 status_line.items）原样保留；tui.toml 不存在时新建。
///
/// 注释会随重写丢失（可接受：Kimi Code CLI 自己也会重写该文件）
pub fn install(home: &Path) -> Result<(), String> {
    let path = home.join("tui.toml");
    let mut doc = load_doc(&path)?;
    let status_line = doc
        .entry("status_line".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or_else(|| "tui.toml 的 [status_line] 不是表，未修改".to_string())?;
    let exe = std::env::current_exe().map_err(|e| format!("获取当前程序路径失败: {e}"))?;
    let desired = build_command(&exe.to_string_lossy())?;
    match status_line.get("command").and_then(|v| v.as_str()) {
        // 已是我们的命令且形态一致 → 幂等跳过（不重写文件）
        Some(cmd) if cmd == desired => return Ok(()),
        // 含我们的标记但形态过时（如早期带引号形态）→ 落下去重写到最新形态
        Some(cmd) if cmd.contains(MARKER) => {}
        Some(_) => {
            return Err(
                "tui.toml 已有自定义 status_line.command，为避免覆盖请手动合并".to_string(),
            );
        }
        None => {}
    }
    status_line.insert("command".to_string(), desired.into());
    write_doc(&path, &doc)
}

/// 拼状态栏命令（纯函数）：`<exe> --statusline`，无引号（Kimi Code 按空格切 argv）；
/// 含空格的路径无法寻址也无法用引号补救，直接报错
fn build_command(exe: &str) -> Result<String, String> {
    if exe.contains(char::is_whitespace) {
        return Err(format!(
            "程序路径含空格（{exe}），Kimi Code 状态栏不支持寻址，未安装"
        ));
    }
    Ok(format!("{exe} {MARKER}"))
}

/// 摘除状态栏命令：只摘含我们标记的 command；不是我们的命令 / 无 tui.toml /
/// 无 [status_line] / 无 command 一律不动（不报错、不新建文件）。
/// 其他字段原样保留
pub fn uninstall(home: &Path) -> Result<(), String> {
    let path = home.join("tui.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("读取 {} 失败: {e}", path.display())),
    };
    let mut doc: toml::Table = text
        .parse()
        .map_err(|e| format!("解析 {} 失败: {e}", path.display()))?;
    let Some(status_line) = doc.get_mut("status_line").and_then(|v| v.as_table_mut()) else {
        return Ok(());
    };
    let Some(command) = status_line.get("command") else {
        return Ok(());
    };
    let Some(cmd) = command.as_str() else {
        return Ok(());
    };
    if !cmd.contains(MARKER) {
        return Ok(());
    }
    status_line.remove("command");
    write_doc(&path, &doc)
}

/// 读 tui.toml 为 Table：文件不存在 → 空表（安装时新建）；损坏 → Err
/// （绝不静默覆盖用户数据）
fn load_doc(path: &Path) -> Result<toml::Table, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .parse()
            .map_err(|e| format!("解析 {} 失败: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(toml::Table::new()),
        Err(e) => Err(format!("读取 {} 失败: {e}", path.display())),
    }
}

/// 序列化并写入 tui.toml（无注释保留；字段按字典序重排无妨，Kimi Code CLI 本就会重写）
fn write_doc(path: &Path, doc: &toml::Table) -> Result<(), String> {
    let text = toml::to_string(doc).map_err(|e| format!("序列化 {} 失败: {e}", path.display()))?;
    std::fs::write(path, text).map_err(|e| format!("写入 {} 失败: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;
    use crate::quota::{KimiQuota, QuotaDetail};
    use crate::storage::{CachedQuota, Settings};
    use crate::TEST_ENV_LOCK as ENV_LOCK;

    // ---- 测试构件 ----

    fn account(provider: &str) -> Account {
        Account {
            id: "acc-1".to_string(),
            name: "账号 1".to_string(),
            login_method: None,
            provider: provider.to_string(),
        }
    }

    fn kimi_quota(five_hour_pct: Option<f64>, weekly_pct: Option<f64>) -> KimiQuota {
        KimiQuota {
            five_hour: five_hour_pct.map(|p| QuotaDetail {
                percent_remaining: p,
                ..Default::default()
            }),
            weekly: weekly_pct.map(|p| QuotaDetail {
                percent_remaining: p,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn cache(quota: KimiQuota, fetched_at: i64) -> CachedQuota {
        CachedQuota {
            quota,
            fetched_at,
            monthly: None,
            deepseek_balance: None,
        }
    }

    fn deepseek_balance(
        is_available: bool,
        total: f64,
    ) -> crate::deepseek::models::DeepSeekBalance {
        crate::deepseek::models::DeepSeekBalance {
            is_available,
            currency: "CNY".to_string(),
            total_balance: total,
            granted_balance: 0.0,
            topped_up_balance: total,
        }
    }

    /// 临时目录（测试后由调用方清理）
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "kimicodebar-statusline-test-{tag}-{}",
            uuid::Uuid::new_v4()
        ))
    }

    /// 当前测试进程的 exe（install 写出的 command 应与它一致，无引号、空格切分语义）
    fn current_command() -> String {
        format!(
            "{} {MARKER}",
            std::env::current_exe().unwrap().to_string_lossy()
        )
    }

    // ---- render_line：Kimi / GLM ----

    #[test]
    fn kimi_line_zh_and_en_contract_shape() {
        let mut q = kimi_quota(Some(42.0), Some(18.0));
        q.membership_level = Some("Allegro".to_string());
        let acc = account("kimi");
        // 阈值 20：18% < 20% 会带 ⚠，先用 42/18 验证契约原文；告警前缀见专测
        assert_eq!(
            render_line(
                &acc,
                &cache(q.clone(), 1_900_000_000),
                Lang::Zh,
                10.0,
                1_900_000_000
            ),
            "Kimi 5小时 42% · 7天 18% · Allegro"
        );
        assert_eq!(
            render_line(
                &acc,
                &cache(q, 1_900_000_000),
                Lang::En,
                10.0,
                1_900_000_000
            ),
            "Kimi 5h 42% · 7d 18% · Allegro"
        );
    }

    #[test]
    fn kimi_membership_enum_maps_to_official_tier_name() {
        let mut q = kimi_quota(Some(42.0), None);
        q.membership_level = Some("LEVEL_INTERMEDIATE".to_string());
        let acc = account("kimi");
        // 枚举值映射官方档位名（与面板 MembershipCard 一致）
        assert_eq!(
            render_line(
                &acc,
                &cache(q, 1_900_000_000),
                Lang::Zh,
                10.0,
                1_900_000_000
            ),
            "Kimi 5小时 42% · Allegretto"
        );
        // 未知等级原样显示
        let mut q2 = kimi_quota(Some(42.0), None);
        q2.membership_level = Some("LEVEL_FUTURE".to_string());
        assert_eq!(
            render_line(
                &acc,
                &cache(q2, 1_900_000_000),
                Lang::Zh,
                10.0,
                1_900_000_000
            ),
            "Kimi 5小时 42% · LEVEL_FUTURE"
        );
    }

    #[test]
    fn kimi_skips_missing_segments_and_membership() {
        let acc = account("kimi");
        // 只有五小时段
        assert_eq!(
            render_line(
                &acc,
                &cache(kimi_quota(Some(42.0), None), 1_900_000_000),
                Lang::Zh,
                10.0,
                1_900_000_000
            ),
            "Kimi 5小时 42%"
        );
        // 只有 7 天段、无档位
        assert_eq!(
            render_line(
                &acc,
                &cache(kimi_quota(None, Some(30.0)), 1_900_000_000),
                Lang::En,
                10.0,
                1_900_000_000
            ),
            "Kimi 7d 30%"
        );
        // 两窗与档位全缺 → 空串（调用方按无可展示非零退出）
        assert_eq!(
            render_line(
                &acc,
                &cache(KimiQuota::default(), 1_900_000_000),
                Lang::Zh,
                10.0,
                1_900_000_000
            ),
            ""
        );
    }

    #[test]
    fn kimi_warn_prefix_when_any_window_below_threshold() {
        let acc = account("kimi");
        // 7 天段 18% < 阈值 20：整行前缀 ⚠（五小时段 42% 正常）
        assert_eq!(
            render_line(
                &acc,
                &cache(kimi_quota(Some(42.0), Some(18.0)), 1_900_000_000),
                Lang::Zh,
                20.0,
                1_900_000_000
            ),
            "⚠ Kimi 5小时 42% · 7天 18%"
        );
        // 五小时段低也触发
        assert_eq!(
            render_line(
                &acc,
                &cache(kimi_quota(Some(5.0), Some(90.0)), 1_900_000_000),
                Lang::Zh,
                20.0,
                1_900_000_000
            ),
            "⚠ Kimi 5小时 5% · 7天 90%"
        );
        // 等于阈值不算低（严格小于，与 needs_low_warning 同语义）
        assert_eq!(
            render_line(
                &acc,
                &cache(kimi_quota(Some(20.0), Some(30.0)), 1_900_000_000),
                Lang::Zh,
                20.0,
                1_900_000_000
            ),
            "Kimi 5小时 20% · 7天 30%"
        );
    }

    #[test]
    fn glm_account_renders_kimi_format() {
        // GLM 额度本就映射进 KimiQuota：行首前缀同样 Kimi
        assert_eq!(
            render_line(
                &account("glm"),
                &cache(kimi_quota(Some(42.0), Some(18.0)), 1_900_000_000),
                Lang::Zh,
                10.0,
                1_900_000_000
            ),
            "Kimi 5小时 42% · 7天 18%"
        );
    }

    // ---- render_line：DeepSeek ----

    #[test]
    fn deepseek_balance_line_zh_and_en() {
        let acc = account("deepseek");
        let mut c = cache(KimiQuota::default(), 1_900_000_000);
        c.deepseek_balance = Some(deepseek_balance(true, 123.45));
        assert_eq!(
            render_line(&acc, &c, Lang::Zh, 20.0, 1_900_000_000),
            "DeepSeek 余额 ¥123.45"
        );
        assert_eq!(
            render_line(&acc, &c, Lang::En, 20.0, 1_900_000_000),
            "DeepSeek balance ¥123.45"
        );
    }

    #[test]
    fn deepseek_unavailable_appends_suffix() {
        let acc = account("deepseek");
        let mut c = cache(KimiQuota::default(), 1_900_000_000);
        c.deepseek_balance = Some(deepseek_balance(false, 0.0));
        assert_eq!(
            render_line(&acc, &c, Lang::Zh, 20.0, 1_900_000_000),
            "DeepSeek 余额 ¥0.00 · 不可用"
        );
        assert_eq!(
            render_line(&acc, &c, Lang::En, 20.0, 1_900_000_000),
            "DeepSeek balance ¥0.00 · unavailable"
        );
    }

    #[test]
    fn deepseek_without_balance_renders_empty() {
        // 旧格式缓存无 deepseek_balance 字段：无可展示 → 空串
        let acc = account("deepseek");
        assert_eq!(
            render_line(
                &acc,
                &cache(KimiQuota::default(), 1_900_000_000),
                Lang::Zh,
                20.0,
                1_900_000_000
            ),
            ""
        );
    }

    // ---- render_line：新鲜度后缀 ----

    #[test]
    fn stale_suffix_appends_minutes_or_hours() {
        let acc = account("kimi");
        let base = 1_900_000_000;
        let q = kimi_quota(Some(42.0), None);
        // 严格超过 10 分钟才追加；10 分钟整与 9 分 59 秒都没有
        assert_eq!(
            render_line(&acc, &cache(q.clone(), base), Lang::Zh, 10.0, base + 600),
            "Kimi 5小时 42%"
        );
        assert_eq!(
            render_line(&acc, &cache(q.clone(), base), Lang::Zh, 10.0, base + 599),
            "Kimi 5小时 42%"
        );
        assert_eq!(
            render_line(&acc, &cache(q.clone(), base), Lang::Zh, 10.0, base + 601),
            "Kimi 5小时 42% · 10分钟前"
        );
        assert_eq!(
            render_line(&acc, &cache(q.clone(), base), Lang::Zh, 10.0, base + 3599),
            "Kimi 5小时 42% · 59分钟前"
        );
        // N≥60 显示小时：60 分钟 / 90 分钟都算 1 小时前，120 分钟算 2 小时前
        assert_eq!(
            render_line(&acc, &cache(q.clone(), base), Lang::Zh, 10.0, base + 3600),
            "Kimi 5小时 42% · 1小时前"
        );
        assert_eq!(
            render_line(&acc, &cache(q.clone(), base), Lang::Zh, 10.0, base + 5400),
            "Kimi 5小时 42% · 1小时前"
        );
        assert_eq!(
            render_line(&acc, &cache(q.clone(), base), Lang::Zh, 10.0, base + 7200),
            "Kimi 5小时 42% · 2小时前"
        );
        // 英文形态
        assert_eq!(
            render_line(&acc, &cache(q.clone(), base), Lang::En, 10.0, base + 601),
            "Kimi 5h 42% · 10m ago"
        );
        assert_eq!(
            render_line(&acc, &cache(q, base), Lang::En, 10.0, base + 3600),
            "Kimi 5h 42% · 1h ago"
        );
        // 时钟回拨（now < fetched_at）无后缀
        assert_eq!(
            render_line(
                &acc,
                &cache(kimi_quota(Some(42.0), None), base),
                Lang::Zh,
                10.0,
                base - 100
            ),
            "Kimi 5小时 42%"
        );
    }

    // ---- cli_home ----

    #[test]
    fn cli_home_prefers_kimi_code_home_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("KIMI_CODE_HOME");
        std::env::remove_var("USERPROFILE");
        std::env::remove_var("HOME");
        let root = temp_dir("home");
        std::fs::create_dir_all(&root).unwrap();

        // KIMI_CODE_HOME 有值优先
        std::env::set_var("KIMI_CODE_HOME", &root);
        assert_eq!(cli_home(), Some(root.clone()));

        // 空值视为未设置 → 回退 USERPROFILE
        std::env::set_var("KIMI_CODE_HOME", "");
        std::env::set_var("USERPROFILE", &root);
        std::env::remove_var("HOME");
        assert_eq!(cli_home(), Some(root.join(".kimi-code")));

        // USERPROFILE 缺省时 HOME 兜底
        std::env::remove_var("USERPROFILE");
        std::env::set_var("HOME", &root);
        assert_eq!(cli_home(), Some(root.join(".kimi-code")));

        // 三者全缺 → None
        std::env::remove_var("KIMI_CODE_HOME");
        std::env::remove_var("HOME");
        assert!(cli_home().is_none());

        std::env::remove_var("USERPROFILE");
        let _ = std::fs::remove_dir_all(&root);
    }

    // ---- resolve_account ----

    /// 建 temp 配置目录 + keyring 隔离 + 落盘 settings，返回 (dir, 清理句柄)
    fn setup_env(settings: &Settings) -> std::path::PathBuf {
        let dir = temp_dir("resolve");
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("KIMICODEBAR_CONFIG_DIR", &dir);
        std::env::set_var(
            "KIMICODEBAR_KEYRING_SERVICE",
            format!("KimiCodeBar-test-{}", uuid::Uuid::new_v4()),
        );
        crate::storage::save_settings(settings).unwrap();
        dir
    }

    fn cleanup_env(dir: &std::path::Path, account_ids: &[&str]) {
        for id in account_ids {
            let _ = crate::creds::clear_api_key(id);
        }
        std::env::remove_var("KIMI_CODE_HOME");
        std::env::remove_var("USERPROFILE");
        std::env::remove_var("HOME");
        std::env::remove_var("KIMICODEBAR_CONFIG_DIR");
        std::env::remove_var("KIMICODEBAR_KEYRING_SERVICE");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn resolve_account_no_accounts_returns_none() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = setup_env(&Settings::default());
        assert!(resolve_account(&Settings::default()).is_none());
        cleanup_env(&dir, &[]);
    }

    #[test]
    fn resolve_account_attribution_hit_prefers_attributed_over_first() {
        let _guard = ENV_LOCK.lock().unwrap();
        let mut settings = Settings::default();
        let first = settings.add_account(Some("一号"), "kimi").unwrap();
        let attributed = settings.add_account(Some("二号"), "kimi").unwrap();
        let dir = setup_env(&settings);
        // 归属账号有缓存：写它的 cache-<id>.json
        crate::storage::save_cache(
            &attributed.id,
            &cache(kimi_quota(Some(42.0), None), 1_900_000_000),
        )
        .unwrap();
        // keyring 登记归属账号的 key
        crate::creds::save_api_key(&attributed.id, "test-key").unwrap();
        // CLI home：config.toml 配了同一把 key → 归属到二号
        let home = temp_dir("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("config.toml"),
            "[providers.\"managed:kimi-code\"]\napi_key = \"test-key\"\n",
        )
        .unwrap();
        std::env::set_var("KIMI_CODE_HOME", &home);

        let resolved = resolve_account(&settings).expect("应能选中账号");
        assert_eq!(resolved.id, attributed.id);
        // 归属不中（换一把谁的都不是的 key）→ 回退第一个账号
        std::fs::write(
            home.join("config.toml"),
            "[providers.\"managed:kimi-code\"]\napi_key = \"other-key\"\n",
        )
        .unwrap();
        let resolved = resolve_account(&settings).expect("应能选中账号");
        assert_eq!(resolved.id, first.id);

        std::env::remove_var("KIMI_CODE_HOME");
        let _ = std::fs::remove_dir_all(&home);
        cleanup_env(&dir, &[&attributed.id, &first.id]);
    }

    #[test]
    fn resolve_account_hit_without_cache_falls_back_to_first() {
        let _guard = ENV_LOCK.lock().unwrap();
        let mut settings = Settings::default();
        let first = settings.add_account(Some("一号"), "kimi").unwrap();
        let attributed = settings.add_account(Some("二号"), "kimi").unwrap();
        let dir = setup_env(&settings);
        // 只有一号有缓存
        crate::storage::save_cache(
            &first.id,
            &cache(kimi_quota(Some(42.0), None), 1_900_000_000),
        )
        .unwrap();
        crate::creds::save_api_key(&attributed.id, "test-key").unwrap();
        let home = temp_dir("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("config.toml"),
            "[providers.\"managed:kimi-code\"]\napi_key = \"test-key\"\n",
        )
        .unwrap();
        std::env::set_var("KIMI_CODE_HOME", &home);

        // 归属命中二号但二号无缓存 → 回退一号
        let resolved = resolve_account(&settings).expect("应能选中账号");
        assert_eq!(resolved.id, first.id);

        std::env::remove_var("KIMI_CODE_HOME");
        let _ = std::fs::remove_dir_all(&home);
        cleanup_env(&dir, &[&attributed.id, &first.id]);
    }

    #[test]
    fn resolve_account_oauth_user_id_hit() {
        let _guard = ENV_LOCK.lock().unwrap();
        let mut settings = Settings::default();
        let first = settings.add_account(Some("一号"), "kimi").unwrap();
        let attributed = settings.add_account(Some("OAuth 号"), "kimi").unwrap();
        let dir = setup_env(&settings);
        crate::storage::save_cache(
            &attributed.id,
            &cache(kimi_quota(Some(42.0), None), 1_900_000_000),
        )
        .unwrap();
        // 账号侧 OAuth 凭证：access_token 的 JWT user_id 与 CLI home 一致 → 归属
        let account_token = jwt_token("user-9");
        std::fs::write(
            dir.join(format!("credentials-{}.json", attributed.id)),
            serde_json::json!({ "access_token": account_token }).to_string(),
        )
        .unwrap();
        let home = temp_dir("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(home.join("credentials")).unwrap();
        std::fs::write(
            home.join("credentials").join("kimi-code.json"),
            serde_json::json!({ "access_token": jwt_token("user-9") }).to_string(),
        )
        .unwrap();
        std::env::set_var("KIMI_CODE_HOME", &home);

        let resolved = resolve_account(&settings).expect("应能选中账号");
        assert_eq!(resolved.id, attributed.id);

        std::env::remove_var("KIMI_CODE_HOME");
        let _ = std::fs::remove_dir_all(&home);
        cleanup_env(&dir, &[&attributed.id, &first.id]);
    }

    /// 造一个带 user_id 声明的 JWT（不验签，只解 payload；结构仿 real token 三小段）
    fn jwt_token(user_id: &str) -> String {
        use base64::Engine;
        let encode = |json: &serde_json::Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.to_string())
        };
        format!(
            "{}.{}.sig",
            encode(&serde_json::json!({"alg":"none"})),
            encode(&serde_json::json!({"user_id": user_id}))
        )
    }

    // ---- stdin 快照：解析 / 前缀渲染 / 复合行 ----

    #[test]
    fn parse_snapshot_real_038_shape() {
        // 0.38.0 实机抓取的 stdin 原文
        let text = r#"{"model":"K3-256k","cwd":"C:\\Users\\JYH\\chat-with-Kimi","gitBranch":null,"permissionMode":"manual","planMode":false,"contextUsage":0,"contextTokens":0,"maxContextTokens":262144,"sessionId":"","version":"0.38.0"}"#;
        let snap = parse_snapshot(text).unwrap();
        assert_eq!(snap.model.as_deref(), Some("K3-256k"));
        assert_eq!(snap.cwd.as_deref(), Some(r"C:\Users\JYH\chat-with-Kimi"));
        assert_eq!(snap.git_branch, None);
        assert_eq!(snap.permission_mode.as_deref(), Some("manual"));
        assert!(!snap.plan_mode);
    }

    #[test]
    fn parse_snapshot_tolerates_garbage_empty_and_missing_fields() {
        assert_eq!(parse_snapshot(""), None);
        assert_eq!(parse_snapshot("not json"), None);
        assert_eq!(parse_snapshot("[1,2]"), None);
        // model/cwd 全缺视为无快照
        assert_eq!(parse_snapshot(r#"{"version":"0.38.0"}"#), None);
        // 只有 model 也行
        assert!(parse_snapshot(r#"{"model":"K3-256k"}"#).is_some());
    }

    #[test]
    fn snapshot_prefix_shape_and_noise_reduction() {
        // manual 是默认态不显示；git 分支挂在 cwd 后；yolo/plan 显示
        let snap = Snapshot {
            model: Some("K3-256k".to_string()),
            cwd: Some(r"C:\Users\JYH\proj".to_string()),
            git_branch: Some("main".to_string()),
            permission_mode: Some("yolo".to_string()),
            plan_mode: true,
        };
        assert_eq!(
            render_snapshot_prefix(&snap),
            r"K3-256k · C:\Users\JYH\proj ⎇ main · yolo · plan"
        );
        let manual = Snapshot {
            permission_mode: Some("manual".to_string()),
            ..snap.clone()
        };
        assert_eq!(
            render_snapshot_prefix(&manual),
            r"K3-256k · C:\Users\JYH\proj ⎇ main · plan"
        );
        // 空快照空前缀
        assert_eq!(render_snapshot_prefix(&Snapshot::default()), "");
    }

    #[test]
    fn compose_line_combines_prefix_and_quota() {
        let snap = Snapshot {
            model: Some("K3-256k".to_string()),
            cwd: Some(r"C:\proj".to_string()),
            ..Default::default()
        };
        // 双有：快照在前，│ 分隔
        assert_eq!(
            compose_line(Some(&snap), "Kimi 5小时 83%".to_string()),
            r"K3-256k · C:\proj │ Kimi 5小时 83%"
        );
        // 额度空：仅前缀（保底上屏）
        assert_eq!(
            compose_line(Some(&snap), String::new()),
            r"K3-256k · C:\proj"
        );
        // 无快照：仅额度行
        assert_eq!(
            compose_line(None, "Kimi 5小时 83%".to_string()),
            "Kimi 5小时 83%"
        );
        // 全空：空串（调用方非零退出）
        assert_eq!(compose_line(None, String::new()), "");
    }

    // ---- install / uninstall ----

    #[test]
    fn install_creates_tui_toml_with_our_command() {
        let home = temp_dir("install-create");
        std::fs::create_dir_all(&home).unwrap();
        install(&home).unwrap();
        let text = std::fs::read_to_string(home.join("tui.toml")).unwrap();
        let doc: toml::Table = text.parse().unwrap();
        let command = doc["status_line"]["command"].as_str().unwrap();
        assert_eq!(command, current_command());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn install_idempotent_skips_rewrite() {
        let home = temp_dir("install-idem");
        std::fs::create_dir_all(&home).unwrap();
        install(&home).unwrap();
        let before = std::fs::read_to_string(home.join("tui.toml")).unwrap();
        install(&home).unwrap();
        let after = std::fs::read_to_string(home.join("tui.toml")).unwrap();
        // 幂等跳过：文件内容不变
        assert_eq!(before, after);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn install_migrates_legacy_quoted_command() {
        let home = temp_dir("install-migrate");
        std::fs::create_dir_all(&home).unwrap();
        // 早期带引号形态（Kimi Code 按空格切 argv 会 ENOENT）→ 安装时重写成无引号形态
        let exe = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .to_string();
        std::fs::write(
            home.join("tui.toml"),
            format!("[status_line]\ncommand = '\"{exe}\" --statusline'\n"),
        )
        .unwrap();
        install(&home).unwrap();
        let doc: toml::Table =
            toml::from_str(&std::fs::read_to_string(home.join("tui.toml")).unwrap()).unwrap();
        assert_eq!(
            doc["status_line"]["command"].as_str().unwrap(),
            current_command()
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn install_preserves_items_and_other_sections() {
        let home = temp_dir("install-preserve");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("tui.toml"),
            "[theme]\nbackground = \"red\"\n\n[status_line]\nitems = [\"model\", \"quota\"]\n",
        )
        .unwrap();
        install(&home).unwrap();
        let doc: toml::Table =
            toml::from_str(&std::fs::read_to_string(home.join("tui.toml")).unwrap()).unwrap();
        // 我们只加 command，items 与 theme 原样保留
        let status_line = doc["status_line"].as_table().unwrap();
        assert_eq!(status_line["command"].as_str().unwrap(), current_command());
        assert_eq!(
            status_line["items"].as_array().unwrap(),
            &vec![
                toml::Value::String("model".to_string()),
                toml::Value::String("quota".to_string()),
            ]
        );
        assert_eq!(doc["theme"]["background"].as_str().unwrap(), "red");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn install_conflicts_with_custom_command_and_never_overwrites() {
        let home = temp_dir("install-conflict");
        std::fs::create_dir_all(&home).unwrap();
        let original = "[status_line]\ncommand = \"echo hi\"\nitems = [\"model\"]\n";
        std::fs::write(home.join("tui.toml"), original).unwrap();
        let err = install(&home).unwrap_err();
        assert!(err.contains("自定义"));
        // 文件原样未动
        assert_eq!(
            std::fs::read_to_string(home.join("tui.toml")).unwrap(),
            original
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn install_refuses_corrupt_toml() {
        let home = temp_dir("install-corrupt");
        std::fs::create_dir_all(&home).unwrap();
        let original = "not [ valid toml";
        std::fs::write(home.join("tui.toml"), original).unwrap();
        assert!(install(&home).is_err());
        // 损坏文件绝不被覆盖
        assert_eq!(
            std::fs::read_to_string(home.join("tui.toml")).unwrap(),
            original
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn build_command_unquoted_and_rejects_space_paths() {
        // 无空格路径：无引号直拼（Kimi Code 按空格切 argv，引号会变字面量 ENOENT）
        assert_eq!(
            build_command(r"D:\Apps\KimiCodeBar\kimicodebar.exe").unwrap(),
            format!(r"D:\Apps\KimiCodeBar\kimicodebar.exe {MARKER}")
        );
        // 含空格路径：无法寻址也无法用引号补救，拒装
        assert!(build_command(r"C:\Users\Foo Bar\kimicodebar.exe").is_err());
    }

    #[test]
    fn uninstall_removes_only_our_command() {
        let home = temp_dir("uninstall-ours");
        std::fs::create_dir_all(&home).unwrap();
        // 先装后摘：command 摘掉，items 保留
        std::fs::write(
            home.join("tui.toml"),
            "[status_line]\nitems = [\"model\"]\n",
        )
        .unwrap();
        install(&home).unwrap();
        uninstall(&home).unwrap();
        let doc: toml::Table =
            toml::from_str(&std::fs::read_to_string(home.join("tui.toml")).unwrap()).unwrap();
        let status_line = doc["status_line"].as_table().unwrap();
        assert!(status_line.get("command").is_none());
        assert_eq!(
            status_line["items"].as_array().unwrap(),
            &vec![toml::Value::String("model".to_string())]
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn uninstall_leaves_custom_command_untouched() {
        let home = temp_dir("uninstall-custom");
        std::fs::create_dir_all(&home).unwrap();
        let original = "[status_line]\ncommand = \"echo hi\"\n";
        std::fs::write(home.join("tui.toml"), original).unwrap();
        uninstall(&home).unwrap();
        assert_eq!(
            std::fs::read_to_string(home.join("tui.toml")).unwrap(),
            original
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn uninstall_noop_on_missing_or_unrelated_files() {
        // 无 tui.toml：不报错也不新建
        let home = temp_dir("uninstall-missing");
        std::fs::create_dir_all(&home).unwrap();
        uninstall(&home).unwrap();
        assert!(!home.join("tui.toml").exists());
        // 有 tui.toml 但无 [status_line]：不动
        std::fs::write(home.join("tui.toml"), "[theme]\nbackground = \"red\"\n").unwrap();
        let original = std::fs::read_to_string(home.join("tui.toml")).unwrap();
        uninstall(&home).unwrap();
        assert_eq!(
            std::fs::read_to_string(home.join("tui.toml")).unwrap(),
            original
        );
        // [status_line] 无 command（只有 items）：不动
        std::fs::write(
            home.join("tui.toml"),
            "[status_line]\nitems = [\"model\"]\n",
        )
        .unwrap();
        let original = std::fs::read_to_string(home.join("tui.toml")).unwrap();
        uninstall(&home).unwrap();
        assert_eq!(
            std::fs::read_to_string(home.join("tui.toml")).unwrap(),
            original
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn install_targets_from_cli_homes_enumeration() {
        // 目标 homes = local_usage::cli_homes 枚举结果：默认 home + 横线后缀 home 都装
        let root = temp_dir("cli-homes-install");
        let default_home = root.join(".kimi-code");
        let extra_home = root.join(".kimi-code-work");
        for home in [&default_home, &extra_home] {
            std::fs::create_dir_all(home.join("sessions")).unwrap();
            std::fs::write(home.join("config.toml"), "").unwrap();
        }
        for home in local_usage::cli_homes(&root) {
            install(&home).unwrap();
        }
        for home in [&default_home, &extra_home] {
            let doc: toml::Table =
                toml::from_str(&std::fs::read_to_string(home.join("tui.toml")).unwrap()).unwrap();
            assert_eq!(
                doc["status_line"]["command"].as_str().unwrap(),
                current_command()
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
