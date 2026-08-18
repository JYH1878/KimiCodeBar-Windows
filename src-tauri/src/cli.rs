//! Headless CLI 模式：`--status` 拉取配额并以 JSON 打印后退出，不进入 GUI。
//!
//! 必须在 `tauri::Builder` 之前调用（见 main.rs）：单实例插件会把第二个实例
//! 吞成"唤起已有面板"，CLI 调用一旦走进 Builder 就永远到不了这里。
//!
//! 输出契约（便于脚本 `2>nul` 判断）：
//! - 成功：stdout 打印 pretty JSON，退出码 0；
//! - 失败：stderr 打印错误 JSON，stdout 保持干净，退出码非 0。

use std::io::Write;

use chrono::Utc;
use kimicodebar::creds;
use kimicodebar::kimi::client::KimiClient;
use kimicodebar::kimi::web::{self, MonthlyInfo};
use kimicodebar::quota::KimiQuota;
use serde::Serialize;

/// 退出码：成功
const EXIT_OK: i32 = 0;
/// 退出码：网络 / API 错误（含异步运行时初始化失败）
const EXIT_FETCH_FAILED: i32 = 1;
/// 退出码：无可用凭证
const EXIT_NO_CREDENTIALS: i32 = 2;

/// 成功输出（stdout）：字段 snake_case 与 serde 一致，monthly 缺失为 null
#[derive(Serialize)]
struct StatusOutput {
    ok: bool,
    fetched_at: i64,
    quota: KimiQuota,
    monthly: Option<MonthlyInfo>,
}

/// 失败输出（stderr）
#[derive(Serialize)]
struct ErrorOutput<'a> {
    ok: bool,
    error: &'a str,
}

/// CLI 参数拦截入口：含 `--status` 则执行 CLI 路径并返回 Some(退出码)，
/// 否则返回 None，由 main 继续走 GUI。其他未知参数暂不支持、容忍忽略。
pub fn maybe_run() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !status_requested(&args) {
        return None;
    }
    Some(run_status())
}

/// 参数中是否含 `--status`（同时含其他参数也容忍，只对 --status 响应）
fn status_requested(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--status")
}

/// `--status` 主流程：附着控制台 → 独立 tokio runtime 拉取 → 打印 → 返回退出码
fn run_status() -> i32 {
    // release 是 windows 子系统（无控制台），先尝试附着父控制台让输出可见；
    // 守护对象在本函数返回后 Drop（FreeConsole），必然晚于下面的 flush
    let _console_guard = console::Guard::attach_parent();

    match tokio::runtime::Runtime::new() {
        Ok(rt) => match rt.block_on(fetch_status()) {
            Ok(json) => print_stdout(&json),
            Err((code, json)) => print_stderr(&json, code),
        },
        Err(e) => print_stderr(
            &error_json(&format!("异步运行时初始化失败: {e}")),
            EXIT_FETCH_FAILED,
        ),
    }
}

/// 拉取配额并序列化为成功 JSON；失败返回 (退出码, 错误 JSON)。
/// 多账号：输出第一个账号（保持旧版单账号输出契约不变，见 GOAL 拍板）
async fn fetch_status() -> Result<String, (i32, String)> {
    let first_account = kimicodebar::storage::load_settings()
        .unwrap_or_default()
        .accounts
        .into_iter()
        .next();
    // 凭证解析顺序：环境变量 KIMI_API_KEY（非空）→ 第一个账号的 keyring / OAuth 存储凭证
    let token = match env_token("KIMI_API_KEY") {
        Some(token) => token,
        None => {
            let Some(account) = &first_account else {
                return Err((
                    EXIT_NO_CREDENTIALS,
                    error_json("无可用凭证：请设置 KIMI_API_KEY 环境变量，或先在托盘应用中登录"),
                ));
            };
            match creds::get_active_token(account).await {
                Ok(Some((_kind, token))) => token,
                Ok(None) => {
                    return Err((
                        EXIT_NO_CREDENTIALS,
                        error_json(
                            "无可用凭证：请设置 KIMI_API_KEY 环境变量，或先在托盘应用中登录",
                        ),
                    ));
                }
                Err(e) => {
                    return Err((
                        EXIT_NO_CREDENTIALS,
                        error_json(&format!("读取本地凭证失败: {e}")),
                    ));
                }
            }
        }
    };

    let quota = KimiClient::new()
        .fetch_quota(&token)
        .await
        .map_err(|e| (EXIT_FETCH_FAILED, error_json(&format!("获取配额失败: {e}"))))?;

    // 月度总量是可选增强：无网页凭证或拉取失败都退化为 null，不影响主结果。
    // 新体系（refresh_token）优先自动续期；旧体系 kimi-auth / 环境变量直接当 Bearer 用。
    let account_id = first_account.as_ref().map(|a| a.id.as_str());
    let monthly = match web_monthly(account_id).await {
        Some(info) => Some(info),
        None => match web_token(account_id) {
            Some(token) => web::fetch_subscription_stats(&token).await.ok(),
            None => None,
        },
    };

    Ok(success_json(quota, monthly, Utc::now().timestamp()))
}

/// 新鉴权体系月度拉取：该账号 keyring 有 refresh_token 则自动续期后查询。
/// refresh_token 续期轮换，新值必须写回 keyring（丢旧即失效）。
/// 返回 Some(月度) 表示成功；None 表示未配置 refresh_token 或拉取失败（调用方回退旧路径）。
async fn web_monthly(account_id: Option<&str>) -> Option<MonthlyInfo> {
    let account_id = account_id?;
    let refresh_token = creds::load_web_refresh_token(account_id).ok().flatten()?;
    match web::refresh_access_token(&refresh_token).await {
        Ok(session) => {
            // 轮换后的新 refresh_token 必须落盘，否则旧值失效后无法再续期
            if let Err(e) = creds::save_web_refresh_token(account_id, &session.refresh_token) {
                tracing::warn!("保存轮换后的 refresh_token 失败: {e}");
            }
            // 续期成功留痕（严禁记录 token 本身），便于诊断"自动续期是否在跑"
            tracing::info!("网页 access_token 续期成功");
            web::fetch_subscription_stats(&session.access_token)
                .await
                .ok()
        }
        // refresh_token 已失效：清掉本地凭证，回退旧路径（可能有 kimi-auth / 环境变量）
        Err(web::WebError::Unauthorized) => {
            tracing::warn!("网页 refresh_token 已失效，已清除本地凭证");
            let _ = creds::clear_web_refresh_token(account_id);
            None
        }
        // 网络抖动：暂用旧路径兜底（可能同样失败，月度退化为 null）
        Err(_) => None,
    }
}

/// 旧体系 web token 解析顺序：环境变量 KIMI_WEB_TOKEN（非空）→ 该账号 keyring web_token。
/// keyring 读取失败按"无 web token"处理，不阻断主流程。
/// 仅在新体系 refresh_token 未配置或续期失败时作为兼容路径调用。
fn web_token(account_id: Option<&str>) -> Option<String> {
    if let Some(token) = env_token("KIMI_WEB_TOKEN") {
        return Some(token);
    }
    creds::load_web_token(account_id?).ok().flatten()
}

/// 读取非空环境变量（trim 后为空视为未设置）
fn env_token(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// 成功输出的 pretty JSON（stdout）
fn success_json(quota: KimiQuota, monthly: Option<MonthlyInfo>, fetched_at: i64) -> String {
    serde_json::to_string_pretty(&StatusOutput {
        ok: true,
        fetched_at,
        quota,
        monthly,
    })
    .expect("StatusOutput 序列化不会失败")
}

/// 失败输出的 pretty JSON（stderr）
fn error_json(reason: &str) -> String {
    serde_json::to_string_pretty(&ErrorOutput {
        ok: false,
        error: reason,
    })
    .expect("ErrorOutput 序列化不会失败")
}

/// 打印成功 JSON 到 stdout 并显式 flush（须在 FreeConsole 之前）。
/// 用 writeln! 而非 println!：管道被对端提前关闭时不让宏 panic。
fn print_stdout(json: &str) -> i32 {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{json}");
    let _ = out.flush();
    EXIT_OK
}

/// 打印错误 JSON 到 stderr 并显式 flush，stdout 保持干净
fn print_stderr(json: &str, code: i32) -> i32 {
    let mut err = std::io::stderr();
    let _ = writeln!(err, "{json}");
    let _ = err.flush();
    code
}

// ---------------------------------------------------------------------------
// 控制台附着：release 是 windows 子系统（无控制台），直接 println 在父控制台
// 不可见（重定向/管道则正常）。AttachConsole 到父进程让交互式执行也能看到输出。
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod console {
    /// 附着期间的守护：Drop 时 FreeConsole。附着失败时 attached=false，Drop 空操作。
    pub struct Guard {
        attached: bool,
    }

    impl Guard {
        /// 尝试附着父进程控制台；失败（父进程无控制台等）不致命，
        /// 退化为仅重定向/管道可见。
        pub fn attach_parent() -> Self {
            use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
            // SAFETY: 仅改变进程与控制台的关联，无内存安全副作用；返回 0 表示失败
            let attached = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } != 0;
            Self { attached }
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            if self.attached {
                // SAFETY: 仅在成功附着后调用
                unsafe { windows_sys::Win32::System::Console::FreeConsole() };
            }
        }
    }
}

#[cfg(not(windows))]
mod console {
    /// 非 Windows 平台进程天然继承父控制台，空守护即可
    pub struct Guard;

    impl Guard {
        pub fn attach_parent() -> Self {
            Self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kimicodebar::quota::{QuotaDetail, TotalQuotaInfo};

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn sample_quota() -> KimiQuota {
        KimiQuota {
            weekly: Some(QuotaDetail {
                used: 30.0,
                limit: 100.0,
                remaining: 70.0,
                reset_time: None,
                percent_remaining: 70.0,
            }),
            total: Some(TotalQuotaInfo {
                limit: 500.0,
                remaining: 400.0,
                percent_remaining: 80.0,
            }),
            membership_level: Some("pro".to_string()),
            ..Default::default()
        }
    }

    // ---- status_requested ----

    #[test]
    fn status_flag_detected() {
        assert!(status_requested(&args(&["--status"])));
        // 同时含其他参数也容忍
        assert!(status_requested(&args(&["--verbose", "--status"])));
        assert!(status_requested(&args(&["--status", "--json"])));
    }

    #[test]
    fn no_args_or_other_args_not_cli() {
        assert!(!status_requested(&args(&[])));
        assert!(!status_requested(&args(&["--help"])));
        assert!(!status_requested(&args(&["status"])));
        // 大小写敏感，--STATUS 不命中
        assert!(!status_requested(&args(&["--STATUS"])));
    }

    // ---- success_json ----

    #[test]
    fn success_json_shape() {
        let json = success_json(sample_quota(), None, 1_700_000_000);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["fetched_at"], 1_700_000_000);
        assert!(v["quota"].is_object());
        assert_eq!(v["quota"]["weekly"]["percent_remaining"], 70.0);
        assert_eq!(v["quota"]["membership_level"], "pro");
        assert!(v["monthly"].is_null());
    }

    #[test]
    fn success_json_with_monthly() {
        let monthly = MonthlyInfo {
            total_pct: 25.0,
            kimi_pct: 20.0,
            code_pct: 5.0,
            reset_time: None,
        };
        let json = success_json(sample_quota(), Some(monthly), 42);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["monthly"]["total_pct"], 25.0);
        assert_eq!(v["monthly"]["kimi_pct"], 20.0);
        assert_eq!(v["monthly"]["code_pct"], 5.0);
    }

    // ---- error_json ----

    #[test]
    fn error_json_shape() {
        let json = error_json("无可用凭证");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"], "无可用凭证");
        // 错误输出不带成功侧字段
        assert!(v.get("quota").is_none());
        assert!(v.get("fetched_at").is_none());
    }
}
