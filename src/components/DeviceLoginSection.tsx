import { useCallback, useEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { DeviceLoginState } from "../types";
import {
  cancelDeviceLogin,
  isTauri,
  oauthLogout,
  onDeviceLoginUpdated,
  openExternalUrl,
  startDeviceLogin,
} from "../ipc";

/** 剩余秒数 → "X 分 Y 秒" / "X 分钟" / "Y 秒" */
function formatRemain(sec: number): string {
  const m = Math.floor(sec / 60);
  const s = sec % 60;
  if (m > 0) return s > 0 ? `${m} 分 ${s} 秒` : `${m} 分钟`;
  return `${s} 秒`;
}

interface DeviceLoginSectionProps {
  /** 后端是否已配置 OAuth 凭证 */
  oauthConfigured: boolean;
  /** 登录/退出成功后回调，父组件重新拉取凭证状态 */
  onChanged: () => void;
}

/** 设置页"方式B：账号授权登录"分区（设备码流程状态机） */
export function DeviceLoginSection({ oauthConfigured, onChanged }: DeviceLoginSectionProps) {
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
        <h2 className="scard-title">方式B：账号授权登录</h2>
        <div className="cred-row">
          <span className="badge">已授权登录</span>
        </div>
        {actionError !== null && <p className="hint-err">{actionError}</p>}
        <div className="row-end">
          <button
            type="button"
            className="btn danger"
            onClick={() => void logout()}
            disabled={busy}
          >
            退出登录
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
        <h2 className="scard-title">方式B：账号授权登录</h2>
        <p className="hint-muted">请在浏览器中打开授权页面，输入或确认以下授权码：</p>
        <button
          type="button"
          className="user-code"
          onClick={() => void copyCode()}
          title="点击复制授权码"
        >
          {dl.user_code ?? "—"}
        </button>
        {copied && <p className="hint-ok">已复制</p>}
        {url !== null && (
          <button type="button" className="btn primary wide" onClick={() => void openExternalUrl(url)}>
            打开浏览器授权
          </button>
        )}
        <p className="hint-muted waiting-line">
          <span className="spinner small" />
          等待授权中…
          {remainSec !== null && remainSec > 0 && <span>（剩余约 {formatRemain(remainSec)}）</span>}
          {remainSec === 0 && <span>（授权码已过期，请取消后重试）</span>}
        </p>
        <div className="row-end">
          <button type="button" className="btn" onClick={() => void cancel()}>
            取消
          </button>
        </div>
      </section>
    );
  }

  // 失败：红色错误文案 + 重试
  if (dl !== null && dl.status === "error") {
    return (
      <section className="scard">
        <h2 className="scard-title">方式B：账号授权登录</h2>
        <p className="hint-err">{dl.error ?? "授权登录失败"}</p>
        <div className="row-end">
          <button type="button" className="btn primary" onClick={() => void start()} disabled={busy}>
            重试
          </button>
        </div>
      </section>
    );
  }

  // 默认（未登录）：引导文案 + 开始授权
  return (
    <section className="scard">
      <h2 className="scard-title">方式B：账号授权登录</h2>
      <p className="hint-muted">通过浏览器完成 kimi.com 账号授权，无需手动填写 Key。</p>
      {actionError !== null && <p className="hint-err">{actionError}</p>}
      <div className="row-end">
        <button type="button" className="btn primary" onClick={() => void start()} disabled={busy}>
          {busy ? "正在获取授权码…" : "开始授权登录"}
        </button>
      </div>
    </section>
  );
}
