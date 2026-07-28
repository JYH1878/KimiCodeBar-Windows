import { useCallback, useEffect, useRef, useState, type KeyboardEvent } from "react";
import { useTranslation } from "react-i18next";
import { pauseGlobalHotkey, resumeGlobalHotkey } from "../ipc";

/**
 * 具名键/符号键白名单：KeyboardEvent.code → 展示用主键名。
 * 展示名必须能被后端 hotkey::normalize_hotkey 及 global-hotkey 解析器识别
 * （字母/数字/F 键在 mainKeyName 里按前缀规则处理，不入此表）。
 */
const NAMED_KEY_BY_CODE: Record<string, string> = {
  Space: "Space",
  ArrowUp: "Up",
  ArrowDown: "Down",
  ArrowLeft: "Left",
  ArrowRight: "Right",
  Enter: "Enter",
  Tab: "Tab",
  Backspace: "Backspace",
  Delete: "Delete",
  Insert: "Insert",
  Home: "Home",
  End: "End",
  PageUp: "PageUp",
  PageDown: "PageDown",
  PrintScreen: "PrintScreen",
  ScrollLock: "ScrollLock",
  Pause: "Pause",
  NumLock: "NumLock",
  CapsLock: "CapsLock",
  Minus: "-",
  Equal: "=",
  Comma: ",",
  Period: ".",
  Slash: "/",
  Backslash: "\\",
  Backquote: "`",
  BracketLeft: "[",
  BracketRight: "]",
  Semicolon: ";",
  Quote: "'",
  Numpad0: "Numpad0",
  Numpad1: "Numpad1",
  Numpad2: "Numpad2",
  Numpad3: "Numpad3",
  Numpad4: "Numpad4",
  Numpad5: "Numpad5",
  Numpad6: "Numpad6",
  Numpad7: "Numpad7",
  Numpad8: "Numpad8",
  Numpad9: "Numpad9",
  NumpadAdd: "NumpadAdd",
  NumpadSubtract: "NumpadSubtract",
  NumpadMultiply: "NumpadMultiply",
  NumpadDivide: "NumpadDivide",
  NumpadDecimal: "NumpadDecimal",
  NumpadEnter: "NumpadEnter",
  NumpadEqual: "NumpadEqual",
};

/** 修饰键的 e.key 值集合：按下这些只更新预览，不构成主键 */
const MODIFIER_KEYS = new Set(["Control", "Shift", "Alt", "Meta"]);

/**
 * KeyboardEvent.code → 主键名（布局无关的物理键，Shift 不影响）。
 * 不在白名单（如 ContextMenu、浏览器键）返回 null，由调用方提示"不支持该按键"
 */
function mainKeyName(code: string): string | null {
  const letter = /^Key([A-Z])$/.exec(code);
  if (letter !== null) return letter[1];
  const digit = /^Digit([0-9])$/.exec(code);
  if (digit !== null) return digit[1];
  if (/^F(?:[1-9]|1[0-9]|2[0-4])$/.test(code)) return code;
  return NAMED_KEY_BY_CODE[code] ?? null;
}

/** 由事件修饰键状态计算修饰键段（顺序 Ctrl → Alt → Shift，与既有展示习惯一致） */
function modifierTokens(e: KeyboardEvent): string[] {
  const mods: string[] = [];
  if (e.ctrlKey) mods.push("Ctrl");
  if (e.altKey) mods.push("Alt");
  if (e.shiftKey) mods.push("Shift");
  return mods;
}

interface HotkeyInputProps {
  /** 表单中的当前热键文本（空串表示禁用） */
  value: string;
  /** 录制成功或清除时回调新值（清除为空串） */
  onChange: (value: string) => void;
}

/**
 * 全局热键录制输入框：聚焦后按下组合键即录入（如 Ctrl+Shift+K），
 * 单独 Esc 取消、单独 Backspace/Delete 清除（保存后禁用）。
 * 录制期间临时注销全局热键，否则已注册的组合被系统拦截，本输入框收不到按键。
 */
export function HotkeyInput({ value, onChange }: HotkeyInputProps) {
  const { t } = useTranslation();
  const [recording, setRecording] = useState(false);
  /** 录制中的修饰键预览（如 "Ctrl+Shift+"，主键未定）；无修饰键时为空串走 placeholder */
  const [preview, setPreview] = useState("");
  /** 录制期的短时提示（无修饰键/不支持的键/Win 键组合） */
  const [msg, setMsg] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);
  // 卸载兜底：录制中组件被销毁时也要恢复全局热键；stopRecording 的重入保护也用它
  const recordingRef = useRef(false);

  useEffect(
    () => () => {
      if (recordingRef.current) {
        recordingRef.current = false;
        void resumeGlobalHotkey().catch(() => {});
      }
    },
    [],
  );

  /** 结束录制：恢复全局热键、清预览并移出焦点（使 Tab 导航恢复可用）。重入安全 */
  const stopRecording = useCallback(() => {
    if (!recordingRef.current) return;
    recordingRef.current = false;
    setRecording(false);
    setPreview("");
    void resumeGlobalHotkey().catch(() => {
      // 恢复失败只影响旧热键是否立即生效，保存设置时会重注册，静默即可
    });
    inputRef.current?.blur();
  }, []);

  /** 进入录制：注销全局热键（失败不阻断，仅"按下恰为已注册组合"时会被系统抢走） */
  const startRecording = () => {
    if (recordingRef.current) return;
    recordingRef.current = true;
    setRecording(true);
    setPreview("");
    setMsg(null);
    void pauseGlobalHotkey().catch(() => {});
  };

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (!recordingRef.current) return;
    e.preventDefault();
    e.stopPropagation();
    setMsg(null);
    const bare = !e.ctrlKey && !e.altKey && !e.shiftKey && !e.metaKey;

    // 单独 Esc：取消本次录制（保留原值）
    if (e.key === "Escape" && bare) {
      stopRecording();
      return;
    }
    // 单独 Backspace/Delete：清除热键（空串 → 保存后禁用）
    if ((e.key === "Backspace" || e.key === "Delete") && bare) {
      onChange("");
      stopRecording();
      return;
    }
    // 只按了修饰键：更新预览，等待主键
    if (MODIFIER_KEYS.has(e.key)) {
      const mods = modifierTokens(e);
      setPreview(mods.length > 0 ? `${mods.join("+")}+` : "");
      return;
    }
    // Win 键组合：Chromium 对 Win+xx 的投递不可靠（很多被系统先抢走），明确拒绝而不是录成别的组合
    if (e.metaKey) {
      setMsg(t("settings.general.hotkeyNoWin"));
      return;
    }
    const key = mainKeyName(e.code);
    if (key === null) {
      setMsg(t("settings.general.hotkeyUnsupported"));
      return;
    }
    const mods = modifierTokens(e);
    if (mods.length === 0) {
      // 全局热键必须带修饰键（与后端 normalize_hotkey 的校验一致）
      setMsg(t("settings.general.hotkeyNeedModifier"));
      return;
    }
    onChange([...mods, key].join("+"));
    stopRecording();
  };

  const onKeyUp = (e: KeyboardEvent<HTMLInputElement>) => {
    if (!recordingRef.current) return;
    e.preventDefault();
    // 松开某个修饰键后同步预览（主键在 keydown 已落定，不会走到这）
    const mods = modifierTokens(e);
    setPreview(mods.length > 0 ? `${mods.join("+")}+` : "");
  };

  return (
    <>
      <div className="form-row">
        <label htmlFor="hotkey">{t("settings.general.hotkey")}</label>
        <input
          id="hotkey"
          ref={inputRef}
          className="input hotkey-input"
          type="text"
          readOnly
          placeholder={
            recording
              ? t("settings.general.hotkeyRecordingPlaceholder")
              : t("settings.general.hotkeyPlaceholder")
          }
          value={recording ? preview : value}
          onFocus={startRecording}
          onBlur={stopRecording}
          onKeyDown={onKeyDown}
          onKeyUp={onKeyUp}
          spellCheck={false}
          autoComplete="off"
        />
      </div>
      <p className="hint-muted">{t("settings.general.hotkeyHint")}</p>
      {msg !== null && <p className="hint-err">{msg}</p>}
    </>
  );
}
