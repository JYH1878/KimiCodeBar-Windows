//! 应用更新检查：查询 GitHub Releases 最新版本，并做语义化版本比较。
//!
//! 更新源是作者自己的仓库（JYH1878/KimiCodeBar-Windows）的 GitHub Releases。
//! 版本比较语义参照 `KimiCodeBar-Mac/Windows/src/KimiCodeBar.Core/Services/VersionComparer.cs`。

use std::time::Duration;

use serde::Deserialize;

use crate::kimi::USER_AGENT;

/// GitHub API「最新 Release」接口（固定指向作者仓库）
const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/JYH1878/KimiCodeBar-Windows/releases/latest";
/// 更新检查超时（秒）：独立于配额查询的 30s，设置页不应为更新检查久等
const UPDATE_TIMEOUT_SECS: u64 = 10;
/// Release notes 截断长度（字符数），避免 UI 塞入过长文本
const NOTES_MAX_CHARS: usize = 500;

/// 远端最新 Release 信息
#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    /// 版本标签，如 v0.2.0
    pub tag: String,
    /// Release 页面地址（点击去下载）
    pub url: String,
    /// Release notes（截断 500 字符；无正文为 None）
    pub notes: Option<String>,
}

/// GET GitHub 最新 Release 并解析。UA 用 crate::kimi::USER_AGENT（GitHub API 强制要求 UA），
/// 超时 10s；非 2xx / 网络失败 / 解析失败均返回中文错误文案。
pub async fn fetch_latest_release(http: &reqwest::Client) -> Result<ReleaseInfo, String> {
    let resp = http
        .get(LATEST_RELEASE_URL)
        // 逐请求设置 UA / 超时，传入的 client 无需特化配置
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .timeout(Duration::from_secs(UPDATE_TIMEOUT_SECS))
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                "检查更新超时，请检查网络".to_string()
            } else {
                format!("检查更新失败：{e}")
            }
        })?;

    let status = resp.status();
    if !status.is_success() {
        return Err(format!("检查更新失败：GitHub 返回 {status}"));
    }

    let release: GithubRelease = resp
        .json()
        .await
        .map_err(|e| format!("解析更新信息失败：{e}"))?;

    Ok(ReleaseInfo {
        tag: release.tag_name,
        url: release.html_url,
        notes: release.body.map(|body| truncate_chars(&body, NOTES_MAX_CHARS)),
    })
}

/// latest 是否严格高于 current（语义化版本比较，纯函数）。
///
/// 容忍 v/V 前缀与非数字尾巴（v0.2.0-beta → [0,2,0]；0.2 → [0,2,0]），
/// 逐段数字比较，长度不同时短者补 0；任一无法解析（如 latest 不是版本串）返回 false。
pub fn is_newer(latest: &str, current: &str) -> bool {
    let (Some(latest), Some(current)) = (parse_version(latest), parse_version(current)) else {
        return false;
    };
    let len = latest.len().max(current.len());
    for i in 0..len {
        // 短者补 0：0.2 与 0.2.0 视为相等
        let l = latest.get(i).copied().unwrap_or(0);
        let c = current.get(i).copied().unwrap_or(0);
        if l != c {
            return l > c;
        }
    }
    false
}

/// GitHub API 返回的 Release JSON（只取需要的字段，其余忽略）
#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    body: Option<String>,
}

/// 解析版本串为数字段列表：去首尾空白与 v/V 前缀，按 . 分段，每段取前导数字
///（容忍 -beta 之类的非数字尾巴）；任一段没有前导数字则整体视为非法版本。
fn parse_version(version: &str) -> Option<Vec<u64>> {
    let trimmed = version.trim();
    let stripped = trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
        .unwrap_or(trimmed);
    if stripped.is_empty() {
        return None;
    }
    stripped
        .split('.')
        .map(|seg| {
            let digits: String = seg.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                None
            } else {
                digits.parse::<u64>().ok()
            }
        })
        .collect()
}

/// 按字符数截断（按 char 取，避免切断 UTF-8 序列）
fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- is_newer 版本比较（纯函数，不走网络） ----

    #[test]
    fn v_prefixed_newer_version() {
        assert!(is_newer("v0.2.0", "0.1.0"));
    }

    #[test]
    fn equal_versions_not_newer() {
        assert!(!is_newer("0.1.0", "0.1.0"));
    }

    #[test]
    fn v_prefix_does_not_make_equal_version_newer() {
        assert!(!is_newer("v0.1.0", "0.1.0"));
    }

    #[test]
    fn numeric_segments_not_lexicographic() {
        // 字符串字典序下 "10" < "9"，必须按数字段比较
        assert!(is_newer("0.10.0", "0.9.9"));
    }

    #[test]
    fn short_version_padded_with_zero() {
        // 1.0 → [1,0,0] > [0,99,99]
        assert!(is_newer("1.0", "0.99.99"));
    }

    #[test]
    fn short_equal_to_long_not_newer() {
        // 0.2 与 0.2.0 补零后相等
        assert!(!is_newer("0.2", "0.2.0"));
    }

    #[test]
    fn dirty_latest_returns_false() {
        assert!(!is_newer("not-a-version", "0.1.0"));
        assert!(!is_newer("", "0.1.0"));
        assert!(!is_newer("v", "0.1.0"));
    }

    #[test]
    fn dirty_current_returns_false() {
        // 当前版本异常时保守不提示更新
        assert!(!is_newer("0.2.0", "garbage"));
    }

    #[test]
    fn tolerates_non_numeric_tail() {
        assert!(is_newer("v0.2.0-beta", "0.1.0"));
        // 尾巴不改变段值：与 0.2.0 相等不算更新
        assert!(!is_newer("0.2.0-beta", "0.2.0"));
    }

    #[test]
    fn older_version_not_newer() {
        assert!(!is_newer("0.0.9", "0.1.0"));
    }

    // ---- notes 截断 ----

    #[test]
    fn truncate_chars_respects_limit() {
        assert_eq!(truncate_chars("abcdef", 3), "abc");
        assert_eq!(truncate_chars("abc", 500), "abc");
    }

    #[test]
    fn truncate_chars_counts_chars_not_bytes() {
        // 中文多字节字符按字符截断，不切坏 UTF-8
        let s = "更".repeat(600);
        assert_eq!(truncate_chars(&s, 500).chars().count(), 500);
    }

    // ---- parse_version 解析 ----

    #[test]
    fn parse_version_strips_v_prefix_and_tail() {
        assert_eq!(parse_version("v0.2.0"), Some(vec![0, 2, 0]));
        assert_eq!(parse_version("V1.2.3-rc1"), Some(vec![1, 2, 3]));
        assert_eq!(parse_version(" 0.2 "), Some(vec![0, 2]));
    }

    #[test]
    fn parse_version_rejects_non_version() {
        assert_eq!(parse_version("release-notes"), None);
        assert_eq!(parse_version(""), None);
    }
}
