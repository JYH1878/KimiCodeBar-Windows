import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { CredentialStatus } from "../types";
import { clearApiKey, openExternalUrl, setApiKey } from "../ipc";

/** API Key 控制台地址（kimi.com/code 专用，与开放平台不通用） */
const CONSOLE_URL = "https://www.kimi.com/code/console";

/** 眼睛图标：当前为明文，点击隐藏 */
function EyeIcon() {
  return (
    <svg
      width="15"
      height="15"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" />
      <circle cx="12" cy="12" r="3" />
    </svg>
  );
}

/** 闭眼图标：当前为密文，点击显示明文 */
function EyeOffIcon() {
  return (
    <svg
      width="15"
      height="15"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94" />
      <path d="M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19" />
      <path d="M14.12 14.12a3 3 0 1 1-4.24-4.24" />
      <line x1="1" y1="1" x2="23" y2="23" />
    </svg>
  );
}

interface ApiKeySectionProps {
  /** 目标账号 id（凭证按账号隔离） */
  accountId: string;
  /** 凭证状态（尚未加载完时为 null） */
  status: CredentialStatus | null;
  /** 保存/清除成功后回调，父组件重新拉取凭证状态 */
  onChanged: () => void;
}

/** 设置页"方式A：API Key"分区（按账号配置） */
export function ApiKeySection({ accountId, status, onChanged }: ApiKeySectionProps) {
  const { t } = useTranslation();
  const [keyInput, setKeyInput] = useState("");
  const [showKey, setShowKey] = useState(false);
  const [busy, setBusy] = useState(false);
  const [okMsg, setOkMsg] = useState<string | null>(null);
  const [errMsg, setErrMsg] = useState<string | null>(null);

  const configured = status?.api_key_configured ?? false;
  const masked = status?.api_key_masked ?? null;

  const save = async () => {
    const key = keyInput.trim();
    if (key === "") {
      setErrMsg(t("apiKey.errEmpty"));
      setOkMsg(null);
      return;
    }
    setBusy(true);
    setErrMsg(null);
    setOkMsg(null);
    try {
      await setApiKey(accountId, key);
      setKeyInput("");
      setOkMsg(t("apiKey.saved"));
      onChanged();
    } catch (e) {
      // 后端中文报错原样展示
      setErrMsg(String(e));
    } finally {
      setBusy(false);
    }
  };

  const clear = async () => {
    setBusy(true);
    setErrMsg(null);
    setOkMsg(null);
    try {
      await clearApiKey(accountId);
      setOkMsg(t("apiKey.cleared"));
      onChanged();
    } catch (e) {
      setErrMsg(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="login-body">
      {configured && masked !== null && (
        <div className="cred-row">
          <span className="mono-text">{masked}</span>
          <span className="badge">{t("apiKey.configured")}</span>
        </div>
      )}
      <div className="input-row">
        <input
          className="input grow"
          type={showKey ? "text" : "password"}
          placeholder="sk-kimi-…"
          value={keyInput}
          onChange={(e) => setKeyInput(e.target.value)}
          spellCheck={false}
          autoComplete="off"
        />
        <button
          type="button"
          className="btn icon-btn"
          onClick={() => setShowKey((v) => !v)}
          title={showKey ? t("apiKey.hideKey") : t("apiKey.showKey")}
        >
          {showKey ? <EyeOffIcon /> : <EyeIcon />}
        </button>
      </div>
      <p className="hint-muted">
        {t("apiKey.hint")}
        <br />
        <button type="button" className="link" onClick={() => void openExternalUrl(CONSOLE_URL)}>
          {t("apiKey.getKey")}
        </button>
      </p>
      {errMsg !== null && <p className="hint-err">{errMsg}</p>}
      {okMsg !== null && <p className="hint-ok">{okMsg}</p>}
      <div className="row-end">
        <button
          type="button"
          className="btn danger"
          onClick={() => void clear()}
          disabled={busy || !configured}
        >
          {t("apiKey.clear")}
        </button>
        <button
          type="button"
          className="btn primary"
          onClick={() => void save()}
          disabled={busy || keyInput.trim() === ""}
        >
          {t("apiKey.save")}
        </button>
      </div>
    </div>
  );
}
