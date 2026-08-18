import React, { useCallback, useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { getVersion } from "@tauri-apps/api/app";
import { convertFileSrc } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import "./styles.css";
import i18n, { resolveLang } from "./i18n";
import { applyTheme, useTheme } from "./theme";
import type { AppSettings, ThemeMode, UpdateInfo } from "./types";
import {
  checkUpdate,
  exportDiagnostics,
  exportUsageReport,
  getSettings,
  isTauri,
  listAccounts,
  onSettingsChanged,
  onSettingsNavigate,
  openExternalUrl,
  openLogDir,
  saveSettings,
} from "./ipc";
import { AccountsCard } from "./components/AccountsCard";
import { BackgroundRow } from "./components/BackgroundRow";
import { HotkeyInput } from "./components/HotkeyInput";

/** 预设背景白名单（与后端 background.rs PRESETS / styles.css .bg-<id> 渐变一致；非法 id 按无背景处理） */
const BG_PRESETS = ["night", "aurora", "violet", "ember"];

/** 通用设置表单的本地状态（数字输入框先按字符串持有，保存时解析钳制） */
interface GeneralForm {
  refreshMin: string;
  /** 刷新模式：true=自适应（活跃时 1 分钟，静默按固定间隔），默认 true */
  adaptiveRefresh: boolean;
  lowWarn: boolean;
  threshold: string;
  autostart: boolean;
  /** 全局热键文本（由 HotkeyInput 录制写入，保存时 trim，空串→null 禁用） */
  hotkey: string;
  /** 界面语言（"system"/"zh"/"en"，改动立即本地预览，随保存持久化） */
  language: string;
  /** 主题模式（"system"/"dark"/"light"，改动立即本地预览，随保存持久化） */
  theme: string;
}

/** 设置窗口主界面（settings.html 入口） */
function SettingsApp() {
  const { t } = useTranslation();
  // 主题：读设置 + 跟随 settings-changed + system 模式跟随系统明暗
  useTheme();
  // settings = 最近一次从后端读到/保存成功的设置
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  // 通用设置表单
  const [form, setForm] = useState<GeneralForm>({
    refreshMin: "5",
    adaptiveRefresh: true,
    lowWarn: true,
    threshold: "20",
    autostart: false,
    hotkey: "",
    language: "system",
    theme: "system",
  });
  const [savingGeneral, setSavingGeneral] = useState(false);
  const [generalSaved, setGeneralSaved] = useState(false);
  const [generalError, setGeneralError] = useState<string | null>(null);
  // "已保存" 2 秒自动消失的定时器
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // 诊断与日志：exporting=导出中；exportDone=绿色"已导出"（2 秒自动消失）
  const [exporting, setExporting] = useState(false);
  const [exportDone, setExportDone] = useState(false);
  const [diagError, setDiagError] = useState<string | null>(null);
  // "已导出" 2 秒自动消失的定时器
  const exportTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // 导出用量记录：独立的进行中/完成态与定时器（与诊断导出互不干扰）
  const [exportingUsage, setExportingUsage] = useState(false);
  const [usageExported, setUsageExported] = useState(false);
  const usageTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // 底栏版本号（初始值兼作浏览器 dev 的 mock 回落）
  const [version, setVersion] = useState("0.1.0");
  // 检查更新：checking=请求中；found=有新版（常驻展示，点击去下载）；msg=短时提示（自动消失）
  const [updateChecking, setUpdateChecking] = useState(false);
  const [updateFound, setUpdateFound] = useState<UpdateInfo | null>(null);
  const [updateMsg, setUpdateMsg] = useState<{ kind: "ok" | "err"; text: string } | null>(null);
  const updateTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // 折叠卡片展开态：账号 / 通用设置 / 诊断与日志（默认收起；首装无账号时账号卡自动展开引导添加）
  const [accountsOpen, setAccountsOpen] = useState(false);
  const [generalOpen, setGeneralOpen] = useState(false);
  const [diagOpen, setDiagOpen] = useState(false);
  // 面板「+」定位信号：收到 settings-navigate("account-add") 时递增，账号卡滚动聚焦添加表单
  const [addFocusTick, setAddFocusTick] = useState(0);
  // 预设背景 id（纯 CSS 渐变 class）；null = 未选预设
  const [bgPreset, setBgPreset] = useState<string | null>(null);
  // 自定义背景图（kimibg:// 协议 URL）；null = 无图
  const [bgImage, setBgImage] = useState<string | null>(null);
  // 当前已加载背景（预设 + 文件名）：settings-changed 时按它判断要不要换（防每次保存都重拉图片）
  const bgRef = useRef<{ preset: string | null; image: string | null }>({ preset: null, image: null });

  /** 按设置同步背景（与面板 panel.tsx 同一套逻辑）：预设白名单校验 + 自定义图协议 URL 加版本 query */
  const syncBackground = useCallback((preset: string | null | undefined, filename: string | null | undefined) => {
    const p = preset && BG_PRESETS.includes(preset) ? preset : null;
    const name = filename ?? null;
    if (p === bgRef.current.preset && name === bgRef.current.image) return;
    bgRef.current = { preset: p, image: name };
    setBgPreset(p);
    if (name === null || !isTauri) {
      setBgImage(null);
      return;
    }
    // 同格式换图文件名不变，靠版本 query 让 webview 重新拉取（协议侧已 no-store）
    setBgImage(`${convertFileSrc("bg", "kimibg")}?v=${Date.now()}`);
  }, []);

  /** 背景图上传/清除后重拉设置：保持 settings 状态新鲜，保存通用设置时原样透传不丢字段 */
  const reloadSettings = useCallback(async () => {
    try {
      setSettings(await getSettings());
    } catch {
      // 拉取失败不打断设置页，下次操作时再试
    }
  }, []);

  // 设置窗为持久隐藏复用：组件挂载时重新拉取设置
  useEffect(() => {
    let alive = true;
    (async () => {
      try {
        const [s, accountList] = await Promise.all([getSettings(), listAccounts()]);
        if (!alive) return;
        setSettings(s);
        setForm({
          refreshMin: String(s.refresh_interval_min),
          adaptiveRefresh: s.adaptive_refresh,
          lowWarn: s.low_warn_enabled,
          threshold: String(s.warn_threshold_pct),
          autostart: s.autostart,
          hotkey: s.hotkey ?? "",
          language: s.language ?? "system",
          theme: s.theme ?? "system",
        });
        // 应用持久化的语言（初始渲染用的是系统语言兜底）
        void i18n.changeLanguage(resolveLang(s.language));
        // 背景（预设/图片）与面板同步应用
        syncBackground(s.background_preset, s.background_image);
        // 首装还没有任何账号：账号卡自动展开，引导先添加
        if (accountList.length === 0) setAccountsOpen(true);
      } catch (e) {
        if (alive) setLoadError(String(e));
      }
    })();
    // 跟随后端 save_settings 广播的设置变更即时切换语言与背景
    const unlistenSettings = onSettingsChanged((s) => {
      void i18n.changeLanguage(resolveLang(s.language));
      syncBackground(s.background_preset, s.background_image);
    });
    // 面板「+」等入口的定位请求：展开账号卡并滚动聚焦添加表单
    const unlistenNavigate = onSettingsNavigate((section) => {
      if (section === "account-add") {
        setAccountsOpen(true);
        setAddFocusTick((n) => n + 1);
      }
    });
    return () => {
      alive = false;
      unlistenSettings();
      unlistenNavigate();
    };
  }, [syncBackground]);

  // 底栏版本号动态取自应用清单；浏览器 dev（非 Tauri）保留 mock 回落值
  useEffect(() => {
    if (!isTauri) return;
    getVersion()
      .then((v) => setVersion(v))
      .catch(() => {
        // 拿不到版本号时保持回落值，不影响设置页其他功能
      });
  }, []);

  // 卸载时清掉"已保存"/"已导出"与更新提示的定时器
  useEffect(
    () => () => {
      if (savedTimerRef.current !== null) clearTimeout(savedTimerRef.current);
      if (exportTimerRef.current !== null) clearTimeout(exportTimerRef.current);
      if (usageTimerRef.current !== null) clearTimeout(usageTimerRef.current);
      if (updateTimerRef.current !== null) clearTimeout(updateTimerRef.current);
    },
    [],
  );

  /** 展示一条更新检查的短时提示，到时自动消失 */
  const showUpdateMsg = (kind: "ok" | "err", text: string, ms: number) => {
    setUpdateMsg({ kind, text });
    if (updateTimerRef.current !== null) clearTimeout(updateTimerRef.current);
    updateTimerRef.current = setTimeout(() => setUpdateMsg(null), ms);
  };

  /** 手动检查更新：新版常驻展示下载入口，"已是最新"2 秒消失，失败原因 3 秒消失 */
  const doCheckUpdate = async () => {
    setUpdateChecking(true);
    setUpdateFound(null);
    setUpdateMsg(null);
    try {
      // force=true：手动点击无条件走网络，绕过后端 6h/10min 缓存
      const info = await checkUpdate(true);
      if (info.error !== null) {
        showUpdateMsg("err", info.error, 3000);
      } else if (info.has_update && info.latest !== null) {
        setUpdateFound(info);
      } else {
        showUpdateMsg("ok", t("settings.footer.upToDate"), 2000);
      }
    } catch (e) {
      // invoke 本身抛错（命令未注册等）也按检查失败展示
      showUpdateMsg("err", String(e), 3000);
    } finally {
      setUpdateChecking(false);
    }
  };

  /** 保存通用设置：数字项解析后钳制（间隔 1–60 分钟，阈值 1–99） */
  const saveGeneral = async () => {
    if (settings === null) return;
    const refreshMin = Math.min(60, Math.max(1, Math.floor(Number(form.refreshMin)) || 5));
    const threshold = Math.min(99, Math.max(1, Math.floor(Number(form.threshold)) || 20));
    const next: AppSettings = {
      refresh_interval_min: refreshMin,
      adaptive_refresh: form.adaptiveRefresh,
      low_warn_enabled: form.lowWarn,
      warn_threshold_pct: threshold,
      autostart: form.autostart,
      // 热键 trim 后提交，空串→null 禁用；后端保存时重新注册，冲突会抛中文错误
      hotkey: form.hotkey.trim() === "" ? null : form.hotkey.trim(),
      // 语言随通用设置一起持久化；保存成功后后端广播 settings-changed
      language: form.language,
      // 主题随通用设置一起持久化；保存成功后后端广播 settings-changed，两个窗口即时切换
      theme: form.theme as ThemeMode,
      // 背景（预设/图片）由专属命令直写（BackgroundRow 成功后已 reloadSettings），此处原样透传
      background_image: settings.background_image ?? null,
      background_preset: settings.background_preset ?? null,
    };
    setSavingGeneral(true);
    setGeneralError(null);
    try {
      await saveSettings(next);
      setSettings(next);
      // 回显钳制后的实际值
      setForm((f) => ({ ...f, refreshMin: String(refreshMin), threshold: String(threshold) }));
      setGeneralSaved(true);
      if (savedTimerRef.current !== null) clearTimeout(savedTimerRef.current);
      savedTimerRef.current = setTimeout(() => setGeneralSaved(false), 2000);
    } catch (e) {
      setGeneralError(String(e));
    } finally {
      setSavingGeneral(false);
    }
  };

  /** 语言下拉改动：本地立即切换预览，持久化随"保存设置"一起提交 */
  const changeLanguagePreview = (lang: string) => {
    setForm((f) => ({ ...f, language: lang }));
    void i18n.changeLanguage(resolveLang(lang));
  };

  /** 主题下拉改动：本地立即应用预览，持久化随"保存设置"一起提交 */
  const changeThemePreview = (theme: string) => {
    setForm((f) => ({ ...f, theme }));
    applyTheme(theme as ThemeMode);
  };

  /** 导出诊断文件：成功绿色"已导出"2 秒消失；失败红字原样展示后端错误 */
  const doExportDiagnostics = async () => {    setExporting(true);
    setDiagError(null);
    setExportDone(false);
    try {
      await exportDiagnostics();
      setExportDone(true);
      if (exportTimerRef.current !== null) clearTimeout(exportTimerRef.current);
      exportTimerRef.current = setTimeout(() => setExportDone(false), 2000);
    } catch (e) {
      setDiagError(String(e));
    } finally {
      setExporting(false);
    }
  };

  /** 打开日志目录：失败红字原样展示后端错误 */
  const doOpenLogDir = async () => {
    setDiagError(null);
    try {
      await openLogDir();
    } catch (e) {
      setDiagError(String(e));
    }
  };

  /** 导出用量记录：成功绿色"已导出并打开目录"2 秒消失；失败红字原样展示后端错误 */
  const doExportUsage = async () => {
    setExportingUsage(true);
    setDiagError(null);
    setUsageExported(false);
    try {
      await exportUsageReport();
      setUsageExported(true);
      if (usageTimerRef.current !== null) clearTimeout(usageTimerRef.current);
      usageTimerRef.current = setTimeout(() => setUsageExported(false), 2000);
    } catch (e) {
      setDiagError(String(e));
    } finally {
      setExportingUsage(false);
    }
  };

  // 背景：预设为纯 CSS 渐变 class（底色干净，不压遮罩）；自定义图压固定浓度遮罩。预设优先于图片（与面板一致）
  const bgStyle =
    bgPreset === null && bgImage !== null
      ? { backgroundImage: `linear-gradient(var(--bg-scrim), var(--bg-scrim)), url("${bgImage}")` }
      : undefined;
  const settingsCls = `settings${bgPreset !== null || bgImage !== null ? " has-bg" : ""}${bgPreset !== null ? ` bg-${bgPreset}` : ""}`;

  // 首屏加载中
  if (settings === null && loadError === null) {
    return (
      <div className={`${settingsCls} loading-center`} style={bgStyle}>
        <div className="spinner" />
        <p className="muted-text">{t("settings.loading")}</p>
      </div>
    );
  }

  // 设置加载失败（理论上不应发生）：只显示错误
  if (settings === null) {
    return (
      <div className={`${settingsCls} loading-center`} style={bgStyle}>
        <p className="hint-err">{loadError}</p>
      </div>
    );
  }

  // 发现新版时的发布页地址（仅在 latest 与 release_url 齐备时展示下载入口）
  const foundUrl = updateFound !== null && updateFound.latest !== null
    ? updateFound.release_url
    : null;

  return (
    <div className={settingsCls} style={bgStyle}>
      <h1 className="settings-title">{t("settings.title")}</h1>

      {/* A. 账号管理卡：账号列表（改名/排序/二次确认删除）+ 添加表单 + 按账号的凭证配置区
             （登录方式 / API Key / OAuth / 网页 token 全部在本卡内按账号配置） */}
      <AccountsCard
        open={accountsOpen}
        onToggle={() => setAccountsOpen((v) => !v)}
        addFocusTick={addFocusTick}
      />

      {/* D. 通用设置（折叠卡片） */}
      <section className="scard">
        <button
          type="button"
          className="collapse-head"
          onClick={() => setGeneralOpen((v) => !v)}
          aria-expanded={generalOpen}
        >
          <span className="scard-title">{t("settings.general.title")}</span>
          <span className={`chevron${generalOpen ? " open" : ""}`}>▸</span>
        </button>
        {generalOpen && (
          <>
            <div className="form-row">
              <label htmlFor="refresh-mode">{t("settings.general.refreshMode")}</label>
              <select
                id="refresh-mode"
                className="input"
                value={form.adaptiveRefresh ? "adaptive" : "fixed"}
                onChange={(e) =>
                  setForm((f) => ({ ...f, adaptiveRefresh: e.target.value === "adaptive" }))
                }
              >
                <option value="adaptive">{t("settings.general.refreshModeAdaptive")}</option>
                <option value="fixed">{t("settings.general.refreshModeFixed")}</option>
              </select>
            </div>
            <div className="form-row">
              <label htmlFor="refresh-interval">{t("settings.general.refreshInterval")}</label>
              <input
                id="refresh-interval"
                className="input num-input"
                type="number"
                min={1}
                max={60}
                step={1}
                value={form.refreshMin}
                onChange={(e) => setForm((f) => ({ ...f, refreshMin: e.target.value }))}
              />
            </div>
            <div className="form-row">
              <label htmlFor="low-warn">{t("settings.general.lowWarn")}</label>
              <input
                id="low-warn"
                type="checkbox"
                checked={form.lowWarn}
                onChange={(e) => setForm((f) => ({ ...f, lowWarn: e.target.checked }))}
              />
            </div>
            <div className="form-row">
              <label htmlFor="warn-threshold">{t("settings.general.warnThreshold")}</label>
              <input
                id="warn-threshold"
                className="input num-input"
                type="number"
                min={1}
                max={99}
                step={1}
                value={form.threshold}
                onChange={(e) => setForm((f) => ({ ...f, threshold: e.target.value }))}
              />
            </div>
            <div className="form-row">
              <label htmlFor="autostart">{t("settings.general.autostart")}</label>
              <input
                id="autostart"
                type="checkbox"
                checked={form.autostart}
                onChange={(e) => setForm((f) => ({ ...f, autostart: e.target.checked }))}
              />
            </div>
            <HotkeyInput
              value={form.hotkey}
              onChange={(v) => setForm((f) => ({ ...f, hotkey: v }))}
            />
            <div className="form-row">
              <label htmlFor="language">{t("settings.general.language")}</label>
              <select
                id="language"
                className="input"
                value={form.language}
                onChange={(e) => changeLanguagePreview(e.target.value)}
              >
                <option value="system">{t("settings.general.langSystem")}</option>
                <option value="zh">{t("settings.general.langZh")}</option>
                <option value="en">{t("settings.general.langEn")}</option>
              </select>
            </div>
            <div className="form-row">
              <label htmlFor="theme">{t("settings.general.theme")}</label>
              <select
                id="theme"
                className="input"
                value={form.theme}
                onChange={(e) => changeThemePreview(e.target.value)}
              >
                <option value="system">{t("settings.general.themeSystem")}</option>
                <option value="dark">{t("settings.general.themeDark")}</option>
                <option value="light">{t("settings.general.themeLight")}</option>
              </select>
            </div>
            <BackgroundRow
              preset={settings.background_preset ?? null}
              imageSet={settings.background_image != null}
              onChanged={() => void reloadSettings()}
            />
            {generalError !== null && <p className="hint-err">{generalError}</p>}
            <div className="row-end">
              {generalSaved && <span className="hint-ok">{t("settings.general.saved")}</span>}
              <button
                type="button"
                className="btn primary"
                onClick={() => void saveGeneral()}
                disabled={savingGeneral}
              >
                {t("settings.general.save")}
              </button>
            </div>
          </>
        )}
      </section>

      {/* E. 诊断与日志（折叠卡片） */}
      <section className="scard">
        <button
          type="button"
          className="collapse-head"
          onClick={() => setDiagOpen((v) => !v)}
          aria-expanded={diagOpen}
        >
          <span className="scard-title">{t("settings.diagnostics.title")}</span>
          <span className={`chevron${diagOpen ? " open" : ""}`}>▸</span>
        </button>
        {diagOpen && (
          <>
            <p className="hint-muted">{t("settings.diagnostics.hint")}</p>
            {diagError !== null && <p className="hint-err">{diagError}</p>}
            <div className="row-end">
              {exportDone && <span className="hint-ok">{t("settings.diagnostics.exported")}</span>}
              <button
                type="button"
                className="btn primary"
                onClick={() => void doExportDiagnostics()}
                disabled={exporting}
              >
                {exporting ? t("settings.diagnostics.exporting") : t("settings.diagnostics.export")}
              </button>
              <button type="button" className="btn" onClick={() => void doOpenLogDir()}>
                {t("settings.diagnostics.openLogDir")}
              </button>
            </div>
            <div className="row-end">
              {usageExported && (
                <span className="hint-ok">{t("settings.diagnostics.usageExported")}</span>
              )}
              <button
                type="button"
                className="btn"
                onClick={() => void doExportUsage()}
                disabled={exportingUsage}
              >
                {exportingUsage
                  ? t("settings.diagnostics.exporting")
                  : t("settings.diagnostics.exportUsage")}
              </button>
            </div>
          </>
        )}
      </section>

      {/* F. 底栏：动态版本号 + 检查更新 */}
      <footer className="settings-footer">
        KimiCodeBar v{version}
        {" · "}
        <button
          type="button"
          className="link"
          onClick={() => void doCheckUpdate()}
          disabled={updateChecking}
        >
          {updateChecking ? t("settings.footer.checking") : t("settings.footer.checkUpdate")}
        </button>
        {foundUrl !== null && (
          <>
            {" · "}
            <button
              type="button"
              className="link"
              onClick={() => void openExternalUrl(foundUrl)}
            >
              {t("settings.footer.foundVersion", { version: updateFound?.latest })}
            </button>
          </>
        )}
        {updateMsg !== null && (
          <>
            {" · "}
            <span className={updateMsg.kind === "err" ? "hint-err" : "hint-ok"}>
              {updateMsg.text}
            </span>
          </>
        )}
      </footer>
    </div>
  );
}

createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <SettingsApp />
  </React.StrictMode>,
);
