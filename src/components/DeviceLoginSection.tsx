import { useCallback, useEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import type { DeviceLoginState } from "../types";
import {
  cancelDeviceLogin,
  isTauri,
  oauthLogout,
  onDeviceLoginUpdated,
  openExternalUrl,
  startDeviceLogin,
} from "../ipc";

/** 剩余秒数 → "X 分 Y 秒" / "X 分钟" / "Y 秒"（英文为 "Xm Ys" / "Xm" / "Ys"） */
function formatRemain(sec: number, t: TFunction): string {
  const m = Math.floor(sec / 60);
  const s = sec % 60;
  if (m > 0) {
    return s > 0 ? t("deviceLogin.remainMinSec", { m, s }) : t("deviceLogin.remainMin", { m });
  }
  return t("deviceLogin.remainSec", { s });
}

interface DeviceLoginSectionProps {
  /** 后端是否已配置 OAuth 凭证 */
  oauthConfigured: boolean;
  /** 登录/退出成功后回调，父组件重新拉取凭证状态 */
  onChanged: () => void;
}

/** 设置页"方式B：账号授权登录"分区（设备码流程状态机） */
export function DeviceLoginSection({ oauthConfigured, onChanged }: DeviceLoginSectionProps) {
  const { t } = useTranslation();
  // 设备码流程状态：null/idle=未开始，waiting=等待授权，success/error=终态
  const [deviceLogin, setDeviceLogin] = useState<DeviceLoginState | null>(null);
  const [busy, setBusy] = useState(false);
  const [remainSec, setRemainSec] = useState<number | null>(null);
  const [copied, setCopied] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);

  // 记录"是否等待授权中"，供关闭窗口/组件卸载时取消流程（ref 避免闭包过期）
  const waitingRef = useRef(false);
  waitingRef.current = deviceLogin?.status === "waiting";

  // 订阅后端推送的登录进度，驱动 waiting → success/error 状态机
  useEffect(() => {
    return onDeviceLoginUpdated((state) => {
      setDeviceLogin(state);
      if (state.status === "success") onChanged();
    });
  }, [onChanged]);

  // 设置窗被关闭（实际为隐藏复用）时，取消等待中的授权
  useEffect(() => {
    if (!isTauri) return;
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    getCurrentWindow()
      .onCloseRequested(() => {
        if (waitingRef.current) void cancelDeviceLogin();
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // 组件卸载兜底：取消等待中的授权（含切换到方式A导致的卸载）
  useEffect(
    () => () => {
      if (waitingRef.current) void cancelDeviceLogin();
    },
    [],
  );

  // 等待授权期间按 expires_in 每秒倒计时
  useEffect(() => {
    if (deviceLogin?.status !== "waiting" || deviceLogin.expires_in === null) {
      setRemainSec(null);
      return;
    }
    const deadline = Date.now() + deviceLogin.expires_in * 1000;
    setRemainSec(deviceLogin.expires_in);
    const timer = setInterval(() => {
      const remain = Math.max(0, Math.ceil((deadline - Date.now()) / 1000));
      setRemainSec(remain);
      if (remain <= 0) clearInterval(timer);
    }, 1000);
    return () => clearInterval(timer);
  }, [deviceLogin]);

  const start = useCallback(async () => {
    setBusy(true);
    setActionError(null);
    try {
      setDeviceLogin(await startDeviceLogin());
    } catch (e) {
      // 拿不到设备码（网络错误等）：直接进入 error 态，展示重试
      setDeviceLogin({
        status: "error",
        user_code: null,
        verification_uri: null,
        verification_uri_complete: null,
        expires_in: null,
        error: String(e),
      });
    } finally {
      setBusy(false);
    }
  }, []);

  const cancel = useCallback(async () => {
    try {
      await cancelDeviceLogin();
    } catch {
      // 后端流程可能已结束，取消失败可忽略
    }
    setDeviceLogin(null);
  }, []);

  const logout = useCallback(async () => {
    setBusy(true);
    setActionError(null);
    try {
      await oauthLogout();
      setDeviceLogin(null);
      onChanged();
    } catch (e) {
      setActionError(String(e));
    } finally {
      setBusy(false);
    }
  }, [onChanged]);

  const copyCode = useCallback(async () => {
    if (!deviceLogin?.user_code) return;
    try {
      await navigator.clipboard.writeText(deviceLogin.user_code);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // 剪贴板不可用时不打扰用户
    }
  }, [deviceLogin?.user_code]);

  const dl = deviceLogin;
  // 已授权：本次流程成功，或后端已有 OAuth 凭证且本地无进行中的流程
  const loggedIn =
    dl?.status === "success" || ((dl === null || dl.status === "idle") && oauthConfigured);

  // 已授权登录：状态徽标 + 退出登录
  if (loggedIn) {
    return (
      <section className="scard">
        <h2 className="scard-title">{t("deviceLogin.title")}</h2>
        <div className="cred-row">
          <span className="badge">{t("deviceLogin.signedIn")}</span>
        </div>
        {actionError !== null && <p className="hint-err">{actionError}</p>}
        <div className="row-end">
          <button
            type="button"
            className="btn danger"
            onClick={() => void logout()}
            disabled={busy}
          >
            {t("deviceLogin.logout")}
          </button>
        </div>
      </section>
    );
  }

  // 等待授权：授权码 + 打开浏览器 + 等待动画/倒计时 + 取消
  if (dl !== null && dl.status === "waiting") {
    const url = dl.verification_uri_complete ?? dl.verification_uri;
    return (
      <section className="scard">
        <h2 className="scard-title">{t("deviceLogin.title")}</h2>
        <p className="hint-muted">{t("deviceLogin.waitingHint")}</p>
        <button
          type="button"
          className="user-code"
          onClick={() => void copyCode()}
          title={t("deviceLogin.copyTitle")}
        >
          {dl.user_code ?? "—"}
        </button>
        {copied && <p className="hint-ok">{t("deviceLogin.copied")}</p>}
        {url !== null && (
          <button type="button" className="btn primary wide" onClick={() => void openExternalUrl(url)}>
            {t("deviceLogin.openBrowser")}
          </button>
        )}
        <p className="hint-muted waiting-line">
          <span className="spinner small" />
          {t("deviceLogin.waiting")}
          {remainSec !== null && remainSec > 0 && (
            <span>{t("deviceLogin.remainAbout", { remain: formatRemain(remainSec, t) })}</span>
          )}
          {remainSec === 0 && <span>{t("deviceLogin.expired")}</span>}
        </p>
        <div className="row-end">
          <button type="button" className="btn" onClick={() => void cancel()}>
            {t("deviceLogin.cancel")}
          </button>
        </div>
      </section>
    );
  }

  // 失败：红色错误文案 + 重试
  if (dl !== null && dl.status === "error") {
    return (
      <section className="scard">
        <h2 className="scard-title">{t("deviceLogin.title")}</h2>
        <p className="hint-err">{dl.error ?? t("deviceLogin.failed")}</p>
        <div className="row-end">
          <button type="button" className="btn primary" onClick={() => void start()} disabled={busy}>
            {t("deviceLogin.retry")}
          </button>
        </div>
      </section>
    );
  }

  // 默认（未登录）：引导文案 + 开始授权
  return (
    <section className="scard">
      <h2 className="scard-title">{t("deviceLogin.title")}</h2>
      <p className="hint-muted">{t("deviceLogin.introHint")}</p>
      {actionError !== null && <p className="hint-err">{actionError}</p>}
      <div className="row-end">
        <button type="button" className="btn primary" onClick={() => void start()} disabled={busy}>
          {busy ? t("deviceLogin.starting") : t("deviceLogin.start")}
        </button>
      </div>
    </section>
  );
}
