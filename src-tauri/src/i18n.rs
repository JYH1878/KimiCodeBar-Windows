//! 后端文案的中英双语查表：不引 i18n crate，仅覆盖系统通知与托盘 tooltip 约 10 条文案。
//!
//! 语言解析优先级：设置项显式 "zh" / "en" → 对应语言；
//! 其他（"system" / None / 未知值）→ 读系统区域（GetUserDefaultLocaleName），
//! zh 开头 → 中文，否则英文。

use kimicodebar::quota::KimiQuota;

/// 界面语言（仅后端文案用；前端 i18next 有自己的一份解析）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Zh,
    En,
}

/// 解析设置项为语言：显式 "zh" / "en" 直接生效（大小写敏感，与 types.ts 契约一致）；
/// 其他（"system" / None / 未知值）回落到系统区域探测
pub fn resolve(setting: Option<&str>) -> Lang {
    match setting {
        Some("zh") => Lang::Zh,
        Some("en") => Lang::En,
        _ => system_lang(),
    }
}

/// 系统区域语言：Windows GetUserDefaultLocaleName（如 "zh-CN" / "en-US"），
/// zh 开头 → 中文，否则英文；探测失败按英文（非中文系统给英文更稳妥）
#[cfg(windows)]
fn system_lang() -> Lang {
    use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;

    // LOCALE_NAME_MAX_LENGTH = 85；返回值为含 NUL 终止符的长度，0 表示失败
    let mut buf = [0u16; 85];
    // SAFETY: 缓冲区长度与传入的 cchLocaleName 一致，API 只在缓冲内写入
    let len = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
    if len <= 0 {
        return Lang::En;
    }
    // 剥掉尾部的 NUL 终止符再转字符串
    let slice = &buf[..(len as usize).saturating_sub(1).min(buf.len())];
    if String::from_utf16_lossy(slice).starts_with("zh") {
        Lang::Zh
    } else {
        Lang::En
    }
}

/// 非 Windows 平台无系统区域探测（本应用仅发布 Windows，此处兜底英文）
#[cfg(not(windows))]
fn system_lang() -> Lang {
    Lang::En
}

/// 低额度系统通知标题
pub fn low_warning_title(lang: Lang) -> &'static str {
    match lang {
        Lang::Zh => "KimiCodeBar 额度预警",
        Lang::En => "KimiCodeBar Low Quota Warning",
    }
}

/// 低额度系统通知正文模板（{summary} 占位，摘要由 quota_summary 生成）
fn low_warning_body_template(lang: Lang) -> &'static str {
    match lang {
        Lang::Zh => "{summary}",
        Lang::En => "{summary}",
    }
}

/// 低额度系统通知正文：模板替换 {summary}
pub fn low_warning_body(lang: Lang, summary: &str) -> String {
    low_warning_body_template(lang).replace("{summary}", summary)
}

/// 5h 窗口重置提醒标题
pub fn reset_reminder_title(lang: Lang) -> &'static str {
    match lang {
        Lang::Zh => "KimiCodeBar 重置提醒",
        Lang::En => "KimiCodeBar Reset Reminder",
    }
}

/// 5h 窗口重置提醒正文模板（{remaining}/{limit}/{minutes}/{reset_time} 占位）
fn reset_reminder_body_template(lang: Lang) -> &'static str {
    match lang {
        Lang::Zh => {
            "5h 剩余 {remaining}/{limit}，{minutes} 分钟后重置（{reset_time}），建议把剩余额度用完"
        }
        Lang::En => {
            "5H left {remaining}/{limit}, resets in {minutes} min ({reset_time}) — time to burn the remaining quota"
        }
    }
}

/// 5h 窗口重置提醒正文：remaining/limit 按整数格式化后替换全部占位
pub fn reset_reminder_body(
    lang: Lang,
    remaining: f64,
    limit: f64,
    minutes: i64,
    reset_time: &str,
) -> String {
    reset_reminder_body_template(lang)
        .replace("{remaining}", &format!("{remaining:.0}"))
        .replace("{limit}", &format!("{limit:.0}"))
        .replace("{minutes}", &minutes.to_string())
        .replace("{reset_time}", reset_time)
}

/// 托盘 tooltip / 通知正文共用的窗口摘要，
/// 如 "7天剩余 87% · 5h剩余 36%"（英文 "7D left 87% · 5H left 36%"）。
/// 缺失的窗口跳过；两窗均无数据时返回空串（调用方据此不发通知/不附加行）。
pub fn quota_summary(lang: Lang, quota: &KimiQuota) -> String {
    let mut parts = Vec::new();
    if let Some(weekly) = &quota.weekly {
        parts.push(match lang {
            Lang::Zh => format!("7天剩余 {:.0}%", weekly.percent_remaining),
            Lang::En => format!("7D left {:.0}%", weekly.percent_remaining),
        });
    }
    if let Some(five_hour) = &quota.five_hour {
        parts.push(match lang {
            Lang::Zh => format!("5h剩余 {:.0}%", five_hour.percent_remaining),
            Lang::En => format!("5H left {:.0}%", five_hour.percent_remaining),
        });
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use kimicodebar::quota::QuotaDetail;

    fn quota(weekly_pct: f64, five_hour_pct: f64) -> KimiQuota {
        KimiQuota {
            weekly: Some(QuotaDetail {
                percent_remaining: weekly_pct,
                ..Default::default()
            }),
            five_hour: Some(QuotaDetail {
                percent_remaining: five_hour_pct,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    // ---- resolve：显式设置优先 ----

    #[test]
    fn resolve_explicit_zh_and_en() {
        assert_eq!(resolve(Some("zh")), Lang::Zh);
        assert_eq!(resolve(Some("en")), Lang::En);
    }

    #[test]
    fn resolve_system_none_and_unknown_fall_back_to_system_locale() {
        // 系统探测结果取决于运行机器区域，不可测；这里只验证这些取值都走回落路径
        assert_eq!(resolve(Some("system")), system_lang());
        assert_eq!(resolve(None), system_lang());
        assert_eq!(resolve(Some("fr")), system_lang());
        // 大小写敏感："ZH" 不是契约值，按未知值回落
        assert_eq!(resolve(Some("ZH")), system_lang());
    }

    // ---- 文案：双语存在性 ----

    #[test]
    fn titles_exist_in_both_languages() {
        for lang in [Lang::Zh, Lang::En] {
            assert!(!low_warning_title(lang).is_empty());
            assert!(!reset_reminder_title(lang).is_empty());
        }
        // 英译确实与中文不同（防止漏翻译）
        assert_ne!(low_warning_title(Lang::Zh), low_warning_title(Lang::En));
        assert_ne!(
            reset_reminder_title(Lang::Zh),
            reset_reminder_title(Lang::En)
        );
    }

    // ---- 摘要格式 ----

    #[test]
    fn summary_zh_and_en_formats() {
        let q = quota(87.0, 36.0);
        assert_eq!(quota_summary(Lang::Zh, &q), "7天剩余 87% · 5h剩余 36%");
        assert_eq!(quota_summary(Lang::En, &q), "7D left 87% · 5H left 36%");
    }

    #[test]
    fn summary_skips_missing_windows() {
        let mut q = quota(87.0, 36.0);
        q.five_hour = None;
        assert_eq!(quota_summary(Lang::Zh, &q), "7天剩余 87%");
        q.weekly = None;
        assert_eq!(quota_summary(Lang::En, &q), "");
    }

    // ---- 占位替换 ----

    #[test]
    fn low_warning_body_replaces_summary_placeholder() {
        assert_eq!(low_warning_body(Lang::Zh, "7天剩余 8%"), "7天剩余 8%");
        assert_eq!(low_warning_body(Lang::En, "7D left 8%"), "7D left 8%");
    }

    #[test]
    fn reset_reminder_body_replaces_all_placeholders() {
        let zh = reset_reminder_body(Lang::Zh, 30.0, 100.0, 12, "08-01 14:30");
        assert_eq!(
            zh,
            "5h 剩余 30/100，12 分钟后重置（08-01 14:30），建议把剩余额度用完"
        );

        let en = reset_reminder_body(Lang::En, 30.0, 100.0, 12, "08-01 14:30");
        assert!(en.contains("30/100"));
        assert!(en.contains("12 min"));
        assert!(en.contains("(08-01 14:30)"));
        // 两种语言都不应残留未替换的占位
        assert!(!en.contains('{'));
        assert!(!zh.contains('{'));
    }

    #[test]
    fn reset_reminder_body_rounds_amounts_to_int() {
        // remaining/limit 按 {:.0} 整数格式化
        let body = reset_reminder_body(Lang::En, 29.6, 100.4, 1, "08-01 14:30");
        assert!(body.contains("30/100"));
    }
}
