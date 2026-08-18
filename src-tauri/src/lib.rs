//! KimiCodeBar 核心库：API 接入与配额解析。
//! 托盘/UI 等二进制侧逻辑在 main.rs，本库只放可测试的核心逻辑。

pub mod accounts;
pub mod background;
pub mod creds;
pub mod history;
pub mod kimi;
pub mod local_usage;
pub mod migrate;
pub mod quota;
pub mod storage;
pub mod update;

/// 测试专用：环境变量是进程级全局状态，凡改动 KIMICODEBAR_CONFIG_DIR 的测试
/// （kimi::oauth / storage 等跨模块共用同一把锁）都须持锁串行，避免互相干扰。
#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
