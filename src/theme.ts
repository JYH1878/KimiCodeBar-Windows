// 主题系统：AppSettings.theme（"system"/"dark"/"light"）解析为实际明暗，
// 写到根节点 data-theme 属性，styles.css 的 CSS 变量据此切换。
// 两个入口（panel/settings）挂载时各调用一次 useTheme：
// 读设置应用一次 → 跟随 settings-changed 广播即时切换 →
// "system" 模式下监听系统 prefers-color-scheme 变化。

import { useEffect } from "react";
import type { ThemeMode } from "./types";
import { getSettings, onSettingsChanged } from "./ipc";

/** 系统明暗查询（媒体查询对象复用，避免重复创建） */
const LIGHT_MQ = window.matchMedia("(prefers-color-scheme: light)");

/** 当前生效的主题设置（模块级记忆，系统主题变化时按它重算） */
let currentMode: ThemeMode | null = null;

/** 解析主题设置为实际明暗：显式 dark/light 直取，其余（system/null/未知值）跟随系统 */
export function resolveTheme(mode: ThemeMode | null | undefined): "dark" | "light" {
  if (mode === "dark" || mode === "light") return mode;
  return LIGHT_MQ.matches ? "light" : "dark";
}

/** 按设置应用主题：更新模块记忆并把解析结果写到根节点 data-theme */
export function applyTheme(mode: ThemeMode | null | undefined): void {
  currentMode = mode ?? null;
  document.documentElement.dataset.theme = resolveTheme(mode);
}

/**
 * 入口共用主题钩子：挂载时读设置应用主题，之后订阅 settings-changed 即时切换；
 * 同时监听系统明暗变化（仅当设置为 "system" 时重算才会改变结果）。
 */
export function useTheme(): void {
  useEffect(() => {
    getSettings()
      .then((s) => applyTheme(s.theme))
      .catch(() => applyTheme(null));
    const unlisten = onSettingsChanged((s) => applyTheme(s.theme));
    // 系统明暗变化：按记忆中的设置重算（dark/light 设置下结果不变，幂等无害）
    const onSystemChange = () => applyTheme(currentMode);
    LIGHT_MQ.addEventListener("change", onSystemChange);
    return () => {
      unlisten();
      LIGHT_MQ.removeEventListener("change", onSystemChange);
    };
  }, []);
}
