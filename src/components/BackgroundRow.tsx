import { useRef, useState, type ChangeEvent } from "react";
import { useTranslation } from "react-i18next";
import { convertFileSrc } from "@tauri-apps/api/core";
import { clearBackgroundImage, isTauri, setBackgroundImage, setBackgroundPreset } from "../ipc";

/** 与后端 background.rs 一致的大小上限（前端先拦一道，免得大文件白跑一趟 IPC） */
const MAX_IMAGE_BYTES = 10 * 1024 * 1024;

/** 预设背景（与后端 background.rs PRESETS / styles.css .bg-<id> 渐变一致） */
const PRESETS = [
  { id: "night", labelKey: "settings.general.bgPresetNight" },
  { id: "aurora", labelKey: "settings.general.bgPresetAurora" },
  { id: "violet", labelKey: "settings.general.bgPresetViolet" },
  { id: "ember", labelKey: "settings.general.bgPresetEmber" },
] as const;

interface BackgroundRowProps {
  /** 当前预设 id（settings.background_preset），null = 未选 */
  preset: string | null;
  /** 是否已上传自定义图（settings.background_image != null；选中预设时图保留但不生效） */
  imageSet: boolean;
  /** 选择/上传/清除成功后回调（父组件重拉设置，保持状态新鲜） */
  onChanged: () => void;
}

/**
 * 通用设置里的"面板背景"选择区：无 / 预设渐变色卡 / 自定义图片色卡，单选即时生效。
 * 预设与图片共存互斥（规则见后端 background.rs：预设优先，上传图自动切回自定义）；
 * 自定义图走隐藏 file input（PNG / JPG / WebP ≤10MB），FileReader 读成 base64 交后端嗅探校验。
 * 错误原样展示（后端中文报错）
 */
export function BackgroundRow({ preset, imageSet, onChanged }: BackgroundRowProps) {
  const { t } = useTranslation();
  const fileRef = useRef<HTMLInputElement | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  /** 选预设（p = id）/ 取消预设切回自定义图（p = null）；后端落盘并广播 settings-changed */
  const choose = async (p: string | null) => {
    if (busy) return;
    setBusy(true);
    setErr(null);
    try {
      await setBackgroundPreset(p);
      onChanged();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  /** "无"色卡：preset / image 全清（图片文件一并删除） */
  const clearAll = async () => {
    if (busy) return;
    setBusy(true);
    setErr(null);
    try {
      await clearBackgroundImage();
      onChanged();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  /** 自定义色卡：已有图 → 取消预设切回图；无图 → 打开文件选择 */
  const onCustomClick = () => {
    if (imageSet) {
      void choose(null);
    } else {
      fileRef.current?.click();
    }
  };

  const onFile = (e: ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    // 清空 value：再次选中同一文件也要触发 change
    e.target.value = "";
    if (!file || busy) return;
    setErr(null);
    if (file.size > MAX_IMAGE_BYTES) {
      setErr(t("settings.general.backgroundTooBig"));
      return;
    }
    const reader = new FileReader();
    reader.onload = () => {
      void (async () => {
        setBusy(true);
        try {
          // readAsDataURL 结果形如 "data:image/png;base64,XXXX"，只取逗号后的纯 base64
          const dataUrl = String(reader.result);
          await setBackgroundImage(dataUrl.slice(dataUrl.indexOf(",") + 1));
          onChanged();
        } catch (e2) {
          setErr(String(e2));
        } finally {
          setBusy(false);
        }
      })();
    };
    reader.onerror = () => setErr(t("settings.general.backgroundReadFailed"));
    reader.readAsDataURL(file);
  };

  return (
    <>
      <div className="form-row">
        <label>{t("settings.general.backgroundImage")}</label>
        <div className="bg-swatches">
          <button
            type="button"
            className={`bg-swatch bg-none${preset === null && !imageSet ? " selected" : ""}`}
            title={t("settings.general.backgroundNone")}
            disabled={busy}
            onClick={() => void clearAll()}
          />
          {PRESETS.map(({ id, labelKey }) => (
            <button
              key={id}
              type="button"
              className={`bg-swatch bg-${id}${preset === id ? " selected" : ""}`}
              title={t(labelKey)}
              disabled={busy}
              onClick={() => void choose(id)}
            />
          ))}
          <button
            type="button"
            className={`bg-swatch${preset === null && imageSet ? " selected" : ""}`}
            title={t("settings.general.backgroundCustom")}
            disabled={busy}
            onClick={onCustomClick}
          >
            {imageSet && isTauri ? (
              <img src={convertFileSrc("bg", "kimibg")} alt="" />
            ) : (
              <span className="bg-plus">+</span>
            )}
          </button>
        </div>
        <input
          ref={fileRef}
          type="file"
          accept="image/png,image/jpeg,image/webp"
          style={{ display: "none" }}
          onChange={onFile}
        />
      </div>
      <p className="hint-muted">{t("settings.general.backgroundHint")}</p>
      {err !== null && <p className="hint-err">{err}</p>}
    </>
  );
}
