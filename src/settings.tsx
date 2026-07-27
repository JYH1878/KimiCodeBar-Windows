import React, { useCallback, useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { getVersion } from "@tauri-apps/api/app";
import { useTranslation } from "react-i18next";
import "./styles.css";
import i18n, { resolveLang } from "./i18n";
import type { AppSettings, CredentialStatus, LoginMethod, UpdateInfo } from "./types";
import {
  checkUpdate,
  exportDiagnostics,
  getCredentialStatus,
  getSettings,
  isTauri,
  onSettingsChanged,
  openExternalUrl,
  openLogDir,
  saveSettings,
} from "./ipc";
import { ApiKeySection } from "./components/ApiKeySection";
import { DeviceLoginSection } from "./components/DeviceLoginSection";
import { WebTokenSection } from "./components/WebTokenSection";

/** 通用设置表单的本地状态（数字输入框先按字符串持有，保存时解析钳制） */
interface GeneralForm {
  refreshMin: string;
  lowWarn: boolean;
  threshold: string;
  autostart: boolean;
  /** 全局热键文本（原样持有输入，保存时 trim，空串→null 禁用） */
  hotkey: string;
  /** 界面语言（"system"/"zh"/"en"，改动立即本地预览，随保存持久化） */
  language: string;
}

/** 设置窗口主界面（settings.html 入口） */
function SettingsApp() {
  const { t } = useTranslation();
  // settings = 最近一次从后端读到/保存成功的设置；status = 凭证配置状态
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [status, setStatus] = useState<CredentialStatus | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  // 登录方式选中态（本地控制，初始值由凭证状态推导）
  const [method, setMethod] = useState<LoginMethod>("api_key");
  const [methodMsg, setMethodMsg] = useState<string | null>(null);
  const [methodError, setMethodError] = useState<string | null>(null);
  // 通用设置表单
  const [form, setForm] = useState<GeneralForm>({
    refreshMin: "5",
    lowWarn: true,
    threshold: "20",
    autostart: false,
    hotkey: "",
    language: "system",
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
  // 底栏版本号（初始值兼作浏览器 dev 的 mock 回落）
  const [version, setVersion] = useState("0.1.0");
  // 检查更新：checking=请求中；found=有新版（常驻展示，点击去下载）；msg=短时提示（自动消失）
  const [updateChecking, setUpdateChecking] = useState(false);
  const [updateFound, setUpdateFound] = useState<UpdateInfo | null>(null);
  const [updateMsg, setUpdateMsg] = useState<{ kind: "ok" | "err"; text: string } | null>(null);
  const updateTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  /** 重新拉取凭证状态（Key 保存/清除、OAuth 登录/退出后调用） */
  const reloadStatus = useCallback(async () => {
    try {
      setStatus(await getCredentialStatus());
    } catch {
      // 状态拉取失败不打断设置页，下次操作时再试
    }
  }, []);

  // 设置窗为持久隐藏复用：组件挂载时重新拉取设置与凭证状态
  useEffect(() => {
    let alive = true;
    (async () => {
      try {
        const [s, st] = await Promise.all([getSettings(), getCredentialStatus()]);
        if (!alive) return;
        setSettings(s);
        setStatus(st);
        // 初始登录方式：显式设置 > 已配 API Key > 已配 OAuth > 默认方式A
        setMethod(
          st.login_method ??
            (st.api_key_configured ? "api_key" : st.oauth_configured ? "oauth" : "api_key"),
        );
        setForm({
          refreshMin: String(s.refresh_interval_min),
          lowWarn: s.low_warn_enabled,
          threshold: String(s.warn_threshold_pct),
          autostart: s.autostart,
          hotkey: s.hotkey ?? "",
          language: s.language ?? "system",
        });
        // 应用持久化的语言（初始渲染用的是系统语言兜底）
        void i18n.changeLanguage(resolveLang(s.language));
      } catch (e) {
        if (alive) setLoadError(String(e));
      }
    })();
    // 跟随后端 save_settings 广播的设置变更即时切换语言
    const unlistenSettings = onSettingsChanged((s) =>
      void i18n.changeLanguage(resolveLang(s.language)),
    );
    return () => {
      alive = false;
      unlistenSettings();
    };
  }, []);

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

  /** 切换登录方式：本地选中态立即更新，并把 login_method 持久化 */
  const switchMethod = async (m: LoginMethod) => {
    if (settings === null || m === method) return;
    setMethod(m);
    setMethodMsg(null);
    setMethodError(null);
    try {
      const next: AppSettings = { ...settings, login_method: m };
      await saveSettings(next);
      setSettings(next);
      setMethodMsg(t("settings.loginMethod.switched"));
    } catch (e) {
      setMethodError(String(e));
    }
  };

  /** 保存通用设置：数字项解析后钳制（间隔 ≥1 分钟，阈值 1–99） */
  const saveGeneral = async () => {
    if (settings === null) return;
    const refreshMin = Math.max(1, Math.floor(Number(form.refreshMin)) || 5);
    const threshold = Math.min(99, Math.max(1, Math.floor(Number(form.threshold)) || 20));
    const next: AppSettings = {
      login_method: method,
      refresh_interval_min: refreshMin,
      low_warn_enabled: form.lowWarn,
      warn_threshold_pct: threshold,
      autostart: form.autostart,
      // 热键 trim 后提交，空串→null 禁用；后端保存时重新注册，冲突会抛中文错误
      hotkey: form.hotkey.trim() === "" ? null : form.hotkey.trim(),
      // 语言随通用设置一起持久化；保存成功后后端广播 settings-changed
      language: form.language,
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

  // 首屏加载中
  if (settings === null && loadError === null) {
    return (
      <div className="settings loading-center">
        <div className="spinner" />
        <p className="muted-text">{t("settings.loading")}</p>
      </div>
    );
  }

  // 设置加载失败（理论上不应发生）：只显示错误
  if (settings === null) {
    return (
      <div className="settings loading-center">
        <p className="hint-err">{loadError}</p>
      </div>
    );
  }

  // 发现新版时的发布页地址（仅在 latest 与 release_url 齐备时展示下载入口）
  const foundUrl = updateFound !== null && updateFound.latest !== null
    ? updateFound.release_url
    : null;

  return (
    <div className="settings">
      <h1 className="settings-title">{t("settings.title")}</h1>

      {/* A. 登录方式 */}
      <section className="scard">
        <h2 className="scard-title">{t("settings.loginMethod.title")}</h2>
        <label className={`radio-row${method === "api_key" ? " active" : ""}`}>
          <input
            type="radio"
            name="login-method"
            checked={method === "api_key"}
            onChange={() => void switchMethod("api_key")}
          />
          <span>{t("settings.loginMethod.apiKey")}</span>
        </label>
        <label className={`radio-row${method === "oauth" ? " active" : ""}`}>
          <input
            type="radio"
            name="login-method"
            checked={method === "oauth"}
            onChange={() => void switchMethod("oauth")}
          />
          <span>{t("settings.loginMethod.oauth")}</span>
        </label>
        {methodMsg !== null && <p className="hint-ok">{methodMsg}</p>}
        {methodError !== null && <p className="hint-err">{methodError}</p>}
      </section>

      {/* B/C. 按选中方式展示对应凭证配置区 */}
      {method === "api_key" ? (
        <ApiKeySection status={status} onChanged={reloadStatus} />
      ) : (
        <DeviceLoginSection
          oauthConfigured={status?.oauth_configured ?? false}
          onChanged={reloadStatus}
        />
      )}

      {/* 高级：月度总量（网页 token，可选，默认收起的折叠卡片） */}
      <WebTokenSection
        configured={status?.web_token_configured ?? false}
        onChanged={reloadStatus}
      />

      {/* D. 通用设置 */}
      <section className="scard">
        <h2 className="scard-title">{t("settings.general.title")}</h2>
        <div className="form-row">
          <label htmlFor="refresh-interval">{t("settings.general.refreshInterval")}</label>
          <input
            id="refresh-interval"
            className="input num-input"
            type="number"
            min={1}
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
        <div className="form-row">
          <label htmlFor="hotkey">{t("settings.general.hotkey")}</label>
          <input
            id="hotkey"
            className="input hotkey-input"
            type="text"
            placeholder={t("settings.general.hotkeyPlaceholder")}
            value={form.hotkey}
            onChange={(e) => setForm((f) => ({ ...f, hotkey: e.target.value }))}
            spellCheck={false}
            autoComplete="off"
          />
        </div>
        <p className="hint-muted">{t("settings.general.hotkeyHint")}</p>
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
      </section>

      {/* E. 诊断与日志 */}
      <section className="scard">
        <h2 className="scard-title">{t("settings.diagnostics.title")}</h2>
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
