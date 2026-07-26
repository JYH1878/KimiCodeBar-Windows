//! 文件日志初始化（tracing）：`{config_dir}/logs/` 下按天滚动
//! `kimicodebar.log.YYYY-MM-DD`，最多保留最近 7 个文件。
//!
//! 默认 info 级，可用 `RUST_LOG` 环境变量覆盖（如 `RUST_LOG=debug`）。
//! 任何一步初始化失败都只退回 stderr，绝不 panic（日志不是关键路径）。

use std::path::PathBuf;
use std::sync::OnceLock;

use tracing_subscriber::EnvFilter;

/// 日志目录名（配置目录下）
const LOG_DIR_NAME: &str = "logs";
/// 日志文件名前缀（按天滚动后形如 kimicodebar.log.2026-07-26）
const LOG_FILE_PREFIX: &str = "kimicodebar.log";
/// 滚动文件保留个数（超出后最旧的被清理）
const MAX_LOG_FILES: usize = 7;

/// non_blocking writer 的守护：drop 即停日志线程，须存活到进程结束
static LOG_GUARD: OnceLock<tracing_appender::non_blocking::WorkerGuard> = OnceLock::new();

/// 初始化全局日志订阅者：文件按天滚动；失败时退回 stderr。
/// 进程内只应调用一次（setup 最先执行）。
pub fn init() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    match file_writer() {
        Ok((writer, guard)) => {
            let _ = LOG_GUARD.set(guard);
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(writer)
                // 文件里不需要 ANSI 颜色转义
                .with_ansi(false)
                .init();
        }
        Err(e) => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(std::io::stderr)
                .init();
            tracing::warn!("日志文件初始化失败，退回 stderr: {e}");
        }
    }
}

/// 日志目录：`{config_dir}/logs`（config_dir 支持 KIMICODEBAR_CONFIG_DIR 覆盖）
pub fn log_dir() -> PathBuf {
    kimicodebar::storage::config_dir().join(LOG_DIR_NAME)
}

/// 构建按天滚动的文件 writer（保留最近 MAX_LOG_FILES 个）
fn file_writer() -> Result<
    (
        tracing_appender::non_blocking::NonBlocking,
        tracing_appender::non_blocking::WorkerGuard,
    ),
    String,
> {
    let dir = log_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建日志目录失败: {e}"))?;
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix(LOG_FILE_PREFIX)
        .max_log_files(MAX_LOG_FILES)
        .build(&dir)
        .map_err(|e| format!("创建滚动日志文件失败: {e}"))?;
    Ok(tracing_appender::non_blocking(appender))
}
