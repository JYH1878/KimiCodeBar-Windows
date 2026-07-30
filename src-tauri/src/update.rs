//! 应用更新检查：查询 GitHub Releases 最新版本，并做语义化版本比较。
//!
//! 更新源是作者自己的仓库（JYH1878/KimiCodeBar-Windows）的 GitHub Releases。
//! 三级回退：① releases/latest 短链的 302 Location 头（网页路由，不受匿名 API
//! 60 次/小时/IP 限流影响，共享代理出口下 API 路径常被限流）→ ② GitHub API →
//! ③ ghfast.top 镜像（纯直连，兜底"GitHub 直连不通且无代理"的环境）。
//! client 走 reqwest 的 system-proxy 特性，自动读 Windows 系统代理——用户梯子
//! 什么端口就用什么（端口来自系统设置，代码不含任何写死端口）；TUN 模式梯子在
//! 网卡层接管，与代理设置无关。
//! 版本比较语义参照 `KimiCodeBar-Mac/Windows/src/KimiCodeBar.Core/Services/VersionComparer.cs`。

use std::time::Duration;

use serde::Deserialize;

use crate::kimi::USER_AGENT;

/// GitHub API「最新 Release」接口（固定指向作者仓库），回退路径
const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/JYH1878/KimiCodeBar-Windows/releases/latest";
/// 作者仓库地址：拼官方 tag 页地址用（concat! 只收字面量，下面的 URL 常量无法复用它）
const REPO_URL: &str = "https://github.com/JYH1878/KimiCodeBar-Windows";
/// 「最新 Release」网页短链（主路径）：302 跳转到 tag 页，走网页路由而非 REST API
const LATEST_RELEASE_PAGE_URL: &str =
    "https://github.com/JYH1878/KimiCodeBar-Windows/releases/latest";
/// ghfast.top 镜像的同款短链（第三级回退）：302 Location 为 /https://github.com/...
/// 形态的相对路径（实测），parse_tag_from_location 的 rfind 仍能取出 tag
const MIRROR_PAGE_URL: &str =
    "https://ghfast.top/https://github.com/JYH1878/KimiCodeBar-Windows/releases/latest";
/// 302 Location 中 tag 页的路径前缀，如 /releases/tag/v0.1.1
const TAG_PATH_PREFIX: &str = "/releases/tag/";
/// 更新检查超时（秒）：独立于配额查询的 30s，设置页不应为更新检查久等
const UPDATE_TIMEOUT_SECS: u64 = 10;
/// Release notes 截断长度（字符数），避免 UI 塞入过长文本
const NOTES_MAX_CHARS: usize = 500;
/// GitHub 限流提示（403/429；共享代理出口常见，各级查询路径共用同一文案）
const RATE_LIMIT_MESSAGE: &str = "GitHub 限流，请稍后再试";
/// 302 跳转地址不符合 .../releases/tag/<tag> 形态时的错误文案
const INVALID_LOCATION_MESSAGE: &str = "发布页地址格式异常";

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

/// 更新检查编排入口：302 重定向路径 → GitHub API → ghfast.top 镜像，三级回退。
/// 三者都失败时返回重定向路径的错误——限流场景下后两级几乎必然同样失败，
/// 其错误对用户没有额外信息量。
pub async fn fetch_latest() -> Result<ReleaseInfo, String> {
    // 不自动跟随重定向：302 的 Location 头本身携带版本信息。
    // 对 API 路径无副作用（成功时 200 直达，不涉重定向），两条 GitHub 路径共用一个
    // client（system-proxy 特性使其自动读 Windows 系统代理，梯子端口随用户系统设置）
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("检查更新失败：{e}"))?;
    // 镜像专用 client：纯直连（no_proxy 同时清掉系统代理与环境变量代理）。
    // 走到镜像时 GitHub 直连多半已挂，但镜像国内可达，必须直连访问——否则
    // "梯子关了但系统代理残留"之类的失效配置会把最后一级回退也拖死
    let http_direct = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .map_err(|e| format!("检查更新失败：{e}"))?;
    match fetch_latest_via_redirect(&http).await {
        Ok(info) => Ok(info),
        Err(redirect_err) => match fetch_latest_release(&http).await {
            Ok(info) => Ok(info),
            Err(_) => match fetch_latest_via_mirror(&http_direct).await {
                Ok(info) => Ok(info),
                // 回主路径错误：它是用户实际需要看到的诊断（如限流提示）
                Err(_) => Err(redirect_err),
            },
        },
    }
}

/// 主路径：GET releases/latest 短链，从 302 的 Location 头解析最新 tag。
/// 网页路由拿不到 Release 正文，notes 置 None。
///
/// 注意：传入的 client 必须配置 redirect::Policy::none()，否则 302 被透明
/// 跟随、Location 头丢失，本函数只能拿到 200 的 HTML 而报状态码错误。
pub async fn fetch_latest_via_redirect(http: &reqwest::Client) -> Result<ReleaseInfo, String> {
    let resp = http
        .get(LATEST_RELEASE_PAGE_URL)
        // 与 API 路径一致：逐请求设置 UA / 超时（GitHub 强制要求 UA）
        .header(reqwest::header::USER_AGENT, USER_AGENT)
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
    if status != reqwest::StatusCode::FOUND {
        return Err(github_status_error(status));
    }

    let location = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| INVALID_LOCATION_MESSAGE.to_string())?;

    Ok(ReleaseInfo {
        tag: parse_tag_from_location(location)?,
        // Release 页面地址保留 Location 原值，点击去下载
        url: location.to_string(),
        notes: None,
    })
}

/// 从 302 Location 解析版本 tag（纯函数）：剥掉 query/fragment 与末尾斜杠后，
/// 最后一段为 tag、其前必须是 /releases/tag/ 前缀，否则视为格式异常。
fn parse_tag_from_location(location: &str) -> Result<String, String> {
    let path = location
        .split(['?', '#'])
        .next()
        .unwrap_or(location)
        .trim_end_matches('/');
    let Some(idx) = path.rfind(TAG_PATH_PREFIX) else {
        return Err(INVALID_LOCATION_MESSAGE.to_string());
    };
    let tag = &path[idx + TAG_PATH_PREFIX.len()..];
    // tag 必须非空且不含路径分隔符（挡住 .../tag/a/b 之类的异常形态）
    if tag.is_empty() || tag.contains('/') {
        return Err(INVALID_LOCATION_MESSAGE.to_string());
    }
    Ok(tag.to_string())
}

/// GitHub 非预期状态码 → 面向用户的中文错误：限流（403/429）单独提示，
/// 避免把共享出口限流误导为应用故障
fn github_status_error(status: reqwest::StatusCode) -> String {
    match status {
        reqwest::StatusCode::FORBIDDEN | reqwest::StatusCode::TOO_MANY_REQUESTS => {
            RATE_LIMIT_MESSAGE.to_string()
        }
        _ => format!("GitHub 返回 {status}"),
    }
}

/// 回退路径：GET GitHub API 最新 Release 并解析。UA 用 crate::kimi::USER_AGENT
/// （GitHub API 强制要求 UA），超时 10s；非 2xx / 网络失败 / 解析失败均返回
/// 中文错误文案（403/429 映射为限流提示）。
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
        return Err(github_status_error(status));
    }

    let release: GithubRelease = resp
        .json()
        .await
        .map_err(|e| format!("解析更新信息失败：{e}"))?;

    Ok(ReleaseInfo {
        tag: release.tag_name,
        url: release.html_url,
        notes: release
            .body
            .map(|body| truncate_chars(&body, NOTES_MAX_CHARS)),
    })
}

/// 第三级回退：GET ghfast.top 镜像的 releases/latest 短链，同样从 302 Location
/// 解析 tag（镜像把 Location 改写为 /https://github.com/... 相对路径，
/// parse_tag_from_location 兼容该形态）。镜像拿不到 Release 正文，notes 置 None。
///
/// 传入的 client 必须是 no_proxy 的纯直连 client（见 fetch_latest）。镜像只用于
/// 取版本号：Release 页地址不信任镜像返回值，用 REPO_URL 自行拼 GitHub 官方地址，
/// 用户点击去下载始终落在 github.com。
pub async fn fetch_latest_via_mirror(http: &reqwest::Client) -> Result<ReleaseInfo, String> {
    let resp = http
        .get(MIRROR_PAGE_URL)
        // 与 GitHub 路径一致：逐请求设置 UA / 超时
        .header(reqwest::header::USER_AGENT, USER_AGENT)
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
    if status != reqwest::StatusCode::FOUND {
        return Err(github_status_error(status));
    }

    let location = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| INVALID_LOCATION_MESSAGE.to_string())?;

    let tag = parse_tag_from_location(location)?;
    Ok(ReleaseInfo {
        url: format!("{REPO_URL}/releases/tag/{tag}"),
        tag,
        notes: None,
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

    // ---- 302 Location tag 解析（纯函数，不走网络） ----

    #[test]
    fn parse_tag_from_standard_location() {
        assert_eq!(
            parse_tag_from_location(
                "https://github.com/JYH1878/KimiCodeBar-Windows/releases/tag/v0.1.1"
            )
            .unwrap(),
            "v0.1.1"
        );
    }

    #[test]
    fn parse_tag_tolerates_query_and_fragment_tail() {
        assert_eq!(
            parse_tag_from_location(
                "https://github.com/JYH1878/KimiCodeBar-Windows/releases/tag/v0.2.0?expanded=true"
            )
            .unwrap(),
            "v0.2.0"
        );
        assert_eq!(
            parse_tag_from_location(
                "https://github.com/JYH1878/KimiCodeBar-Windows/releases/tag/v0.2.0#notes"
            )
            .unwrap(),
            "v0.2.0"
        );
    }

    #[test]
    fn parse_tag_from_mirror_relative_location() {
        // ghfast.top 镜像把 302 Location 改写为 /https://github.com/... 相对路径
        //（实测形态），rfind 前缀仍能取出 tag
        assert_eq!(
            parse_tag_from_location(
                "/https://github.com/JYH1878/KimiCodeBar-Windows/releases/tag/v0.8.0"
            )
            .unwrap(),
            "v0.8.0"
        );
    }

    #[test]
    fn parse_tag_rejects_non_tag_path() {
        // releases 列表页 / 仓库首页等非 tag 地址一律格式异常
        assert!(
            parse_tag_from_location("https://github.com/JYH1878/KimiCodeBar-Windows/releases")
                .is_err()
        );
        assert!(parse_tag_from_location("https://github.com/JYH1878/KimiCodeBar-Windows").is_err());
        // tag 为空或 tag 中再含路径段也是异常形态
        assert!(parse_tag_from_location(
            "https://github.com/JYH1878/KimiCodeBar-Windows/releases/tag/"
        )
        .is_err());
        assert!(parse_tag_from_location(
            "https://github.com/JYH1878/KimiCodeBar-Windows/releases/tag/a/b"
        )
        .is_err());
    }

    #[test]
    fn parse_tag_rejects_empty_location() {
        assert_eq!(
            parse_tag_from_location("").unwrap_err(),
            INVALID_LOCATION_MESSAGE
        );
    }

    // ---- 状态码 → 错误文案映射 ----

    #[test]
    fn status_error_maps_rate_limit() {
        // 403（匿名限额耗尽）与 429 都映射为限流提示
        assert_eq!(
            github_status_error(reqwest::StatusCode::FORBIDDEN),
            RATE_LIMIT_MESSAGE
        );
        assert_eq!(
            github_status_error(reqwest::StatusCode::TOO_MANY_REQUESTS),
            RATE_LIMIT_MESSAGE
        );
    }

    #[test]
    fn status_error_falls_through_to_status_text() {
        assert_eq!(
            github_status_error(reqwest::StatusCode::INTERNAL_SERVER_ERROR),
            "GitHub 返回 500 Internal Server Error"
        );
    }
}
