import React, { useCallback, useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { getVersion } from "@tauri-apps/api/app";
import "./styles.css";
import type { AppSettings, CredentialStatus, LoginMethod, UpdateInfo } from "./types";
import {
  checkUpdate,
  getCredentialStatus,
  getSettings,
  isTauri,
  openExternalUrl,
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
}

/** 设置窗口主界面（settings.html 入口） */
function SettingsApp() {
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
  });
  const [savingGeneral, setSavingGeneral] = useState(false);
  const [generalSaved, setGeneralSaved] = useState(false);
  const [generalError, setGeneralError] = useState<string | null>(null);
  // "已保存" 2 秒自动消失的定时器
  const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
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
        });
      } catch (e) {
        if (alive) setLoadError(String(e));
      }
    })();
    return () => {
      alive = false;
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

  // 卸载时清掉"已保存"与更新提示的定时器
  useEffect(
    () => () => {
      if (savedTimerRef.current !== null) clearTimeout(savedTimerRef.current);
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
        showUpdateMsg("ok", "已是最新", 2000);
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
      setMethodMsg("已切换登录方式，下次刷新生效");
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

  // 首屏加载中
  if (settings === null && loadError === null) {
    return (
      <div className="settings loading-center">
        <div className="spinner" />
        <p className="muted-text">加载中…</p>
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
      <h1 className="settings-title">KimiCodeBar 设置</h1>

      {/* A. 登录方式 */}
      <section className="scard">
        <h2 className="scard-title">登录方式</h2>
        <label className={`radio-row${method === "api_key" ? " active" : ""}`}>
          <input
            type="radio"
            name="login-method"
            checked={method === "api_key"}
            onChange={() => void switchMethod("api_key")}
          />
          <span>方式A：API Key</span>
        </label>
        <label className={`radio-row${method === "oauth" ? " active" : ""}`}>
          <input
            type="radio"
            name="login-method"
            checked={method === "oauth"}
            onChange={() => void switchMethod("oauth")}
          />
          <span>方式B：账号授权登录</span>
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
        <h2 className="scard-title">通用设置</h2>
        <div className="form-row">
          <label htmlFor="refresh-interval">刷新间隔（分钟，≥1）</label>
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
          <label htmlFor="low-warn">低额度告警</label>
          <input
            id="low-warn"
            type="checkbox"
            checked={form.lowWarn}
            onChange={(e) => setForm((f) => ({ ...f, lowWarn: e.target.checked }))}
          />
        </div>
        <div className="form-row">
          <label htmlFor="warn-threshold">告警阈值（剩余 %）</label>
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
          <label htmlFor="autostart">开机自启</label>
          <input
            id="autostart"
            type="checkbox"
            checked={form.autostart}
            onChange={(e) => setForm((f) => ({ ...f, autostart: e.target.checked }))}
          />
        </div>
        {generalError !== null && <p className="hint-err">{generalError}</p>}
        <div className="row-end">
          {generalSaved && <span className="hint-ok">已保存</span>}
          <button
            type="button"
            className="btn primary"
            onClick={() => void saveGeneral()}
            disabled={savingGeneral}
          >
            保存设置
          </button>
        </div>
      </section>

      {/* E. 底栏：动态版本号 + 检查更新 */}
      <footer className="settings-footer">
        KimiCodeBar v{version}
        {" · "}
        <button
          type="button"
          className="link"
          onClick={() => void doCheckUpdate()}
          disabled={updateChecking}
        >
          {updateChecking ? "检查中…" : "检查更新"}
        </button>
        {foundUrl !== null && (
          <>
            {" · "}
            <button
              type="button"
              className="link"
              onClick={() => void openExternalUrl(foundUrl)}
            >
              发现 v{updateFound?.latest}，点击下载
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
