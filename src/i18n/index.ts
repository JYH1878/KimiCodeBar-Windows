// 前端 i18n 框架：i18next + react-i18next。
// zh/en 双资源，fallback 中文；语言来源 = AppSettings.language
// （"zh"/"en" 直取，"system"/null 按 navigator.language 判定）。
// 各入口（panel/settings）挂载时读设置调用 i18n.changeLanguage，
// 并监听后端 save_settings 广播的 settings-changed 事件即时切换。

import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import zh from "./zh.json";
import en from "./en.json";

/** 语言解析：显式 "zh"/"en" 直取；其余（含 system/null/未知值）按浏览器语言是否 zh 开头判定 */
export function resolveLang(setting: string | null | undefined): "zh" | "en" {
  if (setting === "zh" || setting === "en") return setting;
  return navigator.language.toLowerCase().startsWith("zh") ? "zh" : "en";
}

// init 携带内联资源时同步完成，模块被 import 即就绪
void i18n.use(initReactI18next).init({
  resources: {
    zh: { translation: zh },
    en: { translation: en },
  },
  // 初始按系统语言，入口挂载读到设置后再精确切换
  lng: resolveLang(null),
  fallbackLng: "zh",
  interpolation: {
    escapeValue: false, // React 已做 XSS 转义
  },
});

export default i18n;
