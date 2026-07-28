//! 面板背景：预设渐变 + 自定义图片两种，互斥共存。
//!
//! 自定义图片复制进配置目录保存为 `background.<ext>`（不引用用户原始路径，原图被删/移动不受影响），
//! `settings.background_image` 记录文件名；GIF 动图按需求明确拒绝（先把静态图做扎实）。
//! 预设是 `settings.background_preset` 里的 id（纯前端 CSS 渐变，后端只校验白名单并存储）。
//! 生效规则：**preset 优先于 image**——上传图片自动清除 preset（切回自定义），
//! 选预设不动 image（图留着可随时切回），`clear` 两者全清（= 无背景）。
//! 前端拿图走自定义 `kimibg://` 协议（main.rs 注册，处理器调 `load`）：
//! 大图（MB 级）编 base64 过 IPC 再塞 CSS 会断，协议供图零拷贝直出。

use base64::Engine as _;

/// 预设背景白名单（与前端 styles.css 的 .bg-<id> 渐变一一对应，后端只认这几个 id）
pub const PRESETS: [&str; 4] = ["night", "aurora", "violet", "ember"];

/// 背景图片大小上限（字节）：防误选超大原图把配置目录和 IPC 都撑爆
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

/// 支持的静态图片格式（按文件头魔数识别，不信扩展名）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Jpeg,
    Webp,
}

impl ImageKind {
    fn ext(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Webp => "webp",
        }
    }

    fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
        }
    }
}

/// 嗅探图片格式：魔数匹配 PNG / JPG / WebP 返回对应类型；
/// GIF 明确拒绝（动图不做）；其余/过短数据报错。错误均为中文，直接透传前端展示
pub fn sniff(data: &[u8]) -> Result<ImageKind, String> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok(ImageKind::Png);
    }
    if data.starts_with(b"\xff\xd8\xff") {
        return Ok(ImageKind::Jpeg);
    }
    if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return Ok(ImageKind::Webp);
    }
    if data.starts_with(b"GIF8") {
        return Err("暂不支持 GIF 动图，请换静态图片（PNG / JPG / WebP）".to_string());
    }
    Err("不支持的图片格式（仅支持 PNG / JPG / WebP 静态图）".to_string())
}

/// 解码 base64 并保存为背景图片：校验 → 写文件 → 更新 settings → 清理旧文件。
/// 返回保存的文件名（如 "background.png"）
pub fn set_base64(data_base64: &str) -> Result<String, String> {
    let data = base64::engine::general_purpose::STANDARD
        .decode(data_base64.trim())
        .map_err(|_| "图片数据解码失败".to_string())?;
    if data.len() > MAX_IMAGE_BYTES {
        return Err("图片超过 10MB 上限".to_string());
    }
    let kind = sniff(&data)?;
    let filename = format!("background.{}", kind.ext());
    write_atomic(&filename, &data)?;

    let mut settings = crate::storage::load_settings().unwrap_or_default();
    let old = settings.background_image.replace(filename.clone());
    // 上传图片 = 切回自定义模式：清掉预设（图文件本身保留语义不变，只是当前生效源换成图）
    settings.background_preset = None;
    crate::storage::save_settings(&settings)?;
    // 新格式与旧文件不同（如 png → jpg）时删掉旧文件；同文件名刚被覆盖，无需处理
    if let Some(old_name) = old {
        if old_name != filename {
            let _ = std::fs::remove_file(crate::storage::config_dir().join(old_name));
        }
    }
    Ok(filename)
}

/// 清除背景：删除图片文件并把 preset / image 都清空（= 无背景）；未设置过时为无害空操作
pub fn clear() -> Result<(), String> {
    let mut settings = crate::storage::load_settings().unwrap_or_default();
    let old_name = settings.background_image.take();
    settings.background_preset = None;
    crate::storage::save_settings(&settings)?;
    if let Some(old_name) = old_name {
        let _ = std::fs::remove_file(crate::storage::config_dir().join(old_name));
    }
    Ok(())
}

/// 选择预设背景（Some=id，须在白名单内；None=取消预设，切回自定义图/无背景）。
/// 不动 image 字段：上传过的图保留，取消预设即可切回
pub fn set_preset(preset: Option<&str>) -> Result<(), String> {
    if let Some(id) = preset {
        if !PRESETS.contains(&id) {
            return Err(format!("未知的预设背景: {id}"));
        }
    }
    let mut settings = crate::storage::load_settings().unwrap_or_default();
    settings.background_preset = preset.map(str::to_string);
    crate::storage::save_settings(&settings)
}

/// 读取背景图原始字节与 MIME（`kimibg://` 协议处理器直接吐给 webview）；
/// 未设置 / 文件缺失 / 数据损坏均为 None（处理器回 404，前端静默不铺背景）
pub fn load() -> Option<(Vec<u8>, &'static str)> {
    let settings = crate::storage::load_settings().ok()?;
    let filename = settings.background_image?;
    let data = std::fs::read(crate::storage::config_dir().join(filename)).ok()?;
    let kind = sniff(&data).ok()?;
    Some((data, kind.mime()))
}

/// 原子写入（临时文件 + rename，与 storage::save_json 同套路）
fn write_atomic(filename: &str, data: &[u8]) -> Result<(), String> {
    let dir = crate::storage::config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建配置目录失败: {e}"))?;
    let tmp = dir.join(format!("{filename}.tmp"));
    std::fs::write(&tmp, data).map_err(|e| format!("写入背景图片失败: {e}"))?;
    let target = dir.join(filename);
    if target.exists() {
        std::fs::remove_file(&target).map_err(|e| format!("删除旧背景图片失败: {e}"))?;
    }
    std::fs::rename(&tmp, &target).map_err(|e| format!("重命名背景图片失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 环境变量是进程级全局状态，凡改动 KIMICODEBAR_CONFIG_DIR 的测试都须持锁串行
    use crate::TEST_ENV_LOCK as ENV_LOCK;

    /// 各格式的最小文件头（sniff 只看魔数，无需完整图片）
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfake-png-body";
    const JPG: &[u8] = b"\xff\xd8\xff\xe0fake-jpg-body";
    const WEBP: &[u8] = b"RIFF\x0d\x00\x00\x00WEBPfake";
    const GIF: &[u8] = b"GIF89a fake";

    fn use_temp_config_dir() -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("kimicodebar-bg-test-{}", uuid::Uuid::new_v4()));
        std::env::set_var("KIMICODEBAR_CONFIG_DIR", &dir);
        dir
    }

    fn cleanup(dir: &std::path::Path) {
        std::env::remove_var("KIMICODEBAR_CONFIG_DIR");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sniff_recognizes_static_formats() {
        assert_eq!(sniff(PNG).unwrap(), ImageKind::Png);
        assert_eq!(sniff(JPG).unwrap(), ImageKind::Jpeg);
        assert_eq!(sniff(WEBP).unwrap(), ImageKind::Webp);
    }

    #[test]
    fn sniff_rejects_gif_unknown_and_short_data() {
        assert!(sniff(GIF).unwrap_err().contains("GIF"));
        assert!(sniff(b"not an image at all").is_err());
        assert!(sniff(b"").is_err());
        // RIFF 但不是 WEBP（如 WAV）
        assert!(sniff(b"RIFF\x0d\x00\x00\x00WAVEfake").is_err());
    }

    #[test]
    fn set_load_clear_roundtrip_and_format_switch() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_config_dir();
        let enc = base64::engine::general_purpose::STANDARD;

        // 设置 png：文件落盘、settings 记录文件名、临时文件不残留
        let name = set_base64(&enc.encode(PNG)).unwrap();
        assert_eq!(name, "background.png");
        assert!(dir.join("background.png").exists());
        assert!(!dir.join("background.png.tmp").exists());
        assert_eq!(
            crate::storage::load_settings()
                .unwrap()
                .background_image
                .as_deref(),
            Some("background.png")
        );
        let (bytes, mime) = load().expect("应能回读图片字节");
        assert_eq!(mime, "image/png");
        assert_eq!(bytes, PNG);

        // 换成 jpg：旧 png 文件被清掉，回读跟着变
        let name2 = set_base64(&enc.encode(JPG)).unwrap();
        assert_eq!(name2, "background.jpg");
        assert!(!dir.join("background.png").exists());
        assert!(dir.join("background.jpg").exists());
        let (bytes2, mime2) = load().unwrap();
        assert_eq!(mime2, "image/jpeg");
        assert_eq!(bytes2, JPG);

        // 清除：文件与设置项都没了，回读为 None；重复清除是无害空操作
        clear().unwrap();
        assert!(!dir.join("background.jpg").exists());
        assert!(crate::storage::load_settings()
            .unwrap()
            .background_image
            .is_none());
        assert!(load().is_none());
        clear().unwrap();

        cleanup(&dir);
    }

    #[test]
    fn set_rejects_oversize_and_bad_data() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_config_dir();
        let enc = base64::engine::general_purpose::STANDARD;

        let big = vec![0u8; MAX_IMAGE_BYTES + 1];
        assert!(set_base64(&enc.encode(big)).unwrap_err().contains("10MB"));
        assert!(set_base64(&enc.encode(b"plain text")).is_err());
        assert!(set_base64("%%% not base64 %%%").is_err());
        // 全部失败都不应留下背景
        assert!(load().is_none());

        cleanup(&dir);
    }

    #[test]
    fn preset_whitelist_and_mode_switching() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_config_dir();
        let enc = base64::engine::general_purpose::STANDARD;

        // 白名单：四个预设都接受，未知 id 拒绝且不落盘
        for id in PRESETS {
            set_preset(Some(id)).unwrap();
        }
        assert!(set_preset(Some("hacker")).is_err());
        assert_eq!(
            crate::storage::load_settings()
                .unwrap()
                .background_preset
                .as_deref(),
            Some("ember") // 最后一个合法值，未被拒绝的调用覆盖
        );

        // 有预设时上传图片：自动清预设（切回自定义图），图正常落盘
        set_base64(&enc.encode(PNG)).unwrap();
        let s = crate::storage::load_settings().unwrap();
        assert!(s.background_preset.is_none());
        assert_eq!(s.background_image.as_deref(), Some("background.png"));

        // 再选预设：image 字段不动（图保留可随时切回）
        set_preset(Some("night")).unwrap();
        let s = crate::storage::load_settings().unwrap();
        assert_eq!(s.background_preset.as_deref(), Some("night"));
        assert_eq!(s.background_image.as_deref(), Some("background.png"));
        assert!(dir.join("background.png").exists());

        // 取消预设（None）：切回自定义图
        set_preset(None).unwrap();
        assert!(crate::storage::load_settings()
            .unwrap()
            .background_preset
            .is_none());

        // clear：preset / image 全清且删文件（= 无背景）
        set_preset(Some("aurora")).unwrap();
        clear().unwrap();
        let s = crate::storage::load_settings().unwrap();
        assert!(s.background_preset.is_none());
        assert!(s.background_image.is_none());
        assert!(!dir.join("background.png").exists());

        cleanup(&dir);
    }

    #[test]
    fn load_none_when_unset_or_file_missing() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = use_temp_config_dir();

        assert!(load().is_none());

        // settings 有文件名但文件被外部删掉：None 兜底而不是报错
        std::fs::create_dir_all(&dir).unwrap();
        crate::storage::save_settings(&crate::storage::Settings {
            background_image: Some("background.png".to_string()),
            ..Default::default()
        })
        .unwrap();
        assert!(load().is_none());

        cleanup(&dir);
    }
}
