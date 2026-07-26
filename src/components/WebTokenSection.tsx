import { useState } from "react";
import { clearWebToken, openExternalUrl, setWebToken } from "../ipc";

/** Kimi 网页版地址（步骤引导里引导用户登录） */
const KIMI_WEB_URL = "https://www.kimi.com";

interface WebTokenSectionProps {
  /** 后端是否已配置网页 token（CredentialStatus.web_token_configured） */
  configured: boolean;
  /** 保存/清除成功后回调，父组件重新拉取凭证状态 */
  onChanged: () => void;
}

/** 设置页"高级：月度总量（可选）"折叠分区：网页 token 的粘贴校验、保存与清除 */
export function WebTokenSection({ configured, onChanged }: WebTokenSectionProps) {
  // 默认收起，点击标题行展开/收起
  const [open, setOpen] = useState(false);
  const [input, setInput] = useState("");
  // 保存与清除分开记忙态，按钮文案互不干扰
  const [saving, setSaving] = useState(false);
  const [clearing, setClearing] = useState(false);
  const [okMsg, setOkMsg] = useState<string | null>(null);
  const [errMsg, setErrMsg] = useState<string | null>(null);

  const busy = saving || clearing;

  const save = async () => {
    const token = input.trim();
    if (token === "") {
      setErrMsg("请粘贴 kimi-auth 的值");
      setOkMsg(null);
      return;
    }
    setSaving(true);
    setErrMsg(null);
    setOkMsg(null);
    try {
      const info = await setWebToken(token);
      setInput("");
      setOkMsg(
        `已验证并保存：总用量已用 ${info.total_pct.toFixed(1)}%` +
          `（Kimi ${info.kimi_pct.toFixed(1)}% · Code ${info.code_pct.toFixed(1)}%）`,
      );
      onChanged();
    } catch (e) {
      // 后端中文报错原样展示
      setErrMsg(String(e));
    } finally {
      setSaving(false);
    }
  };

  const clear = async () => {
    setClearing(true);
    setErrMsg(null);
    setOkMsg(null);
    try {
      await clearWebToken();
      setOkMsg("网页 token 已清除");
      onChanged();
    } catch (e) {
      setErrMsg(String(e));
    } finally {
      setClearing(false);
    }
  };

  return (
    <section className="scard">
      <button
        type="button"
        className="collapse-head"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
      >
        <span className="scard-title">高级：月度总量（可选）</span>
        {configured && <span className="badge">已配置</span>}
        <span className={`chevron${open ? " open" : ""}`}>▸</span>
      </button>
      {open && (
        <>
          <p className="hint-muted">
            用于显示 Kimi 网页版的每月总用量（Kimi + Code）。需要在浏览器里复制一次网页
            token，过期后需重新粘贴。
          </p>
          <ol className="steps">
            <li>
              浏览器打开{" "}
              <button
                type="button"
                className="link"
                onClick={() => void openExternalUrl(KIMI_WEB_URL)}
              >
                https://www.kimi.com
              </button>{" "}
              并登录
            </li>
            <li>
              按 F12 打开开发者工具 → Application（应用程序）→ Cookies → https://www.kimi.com
            </li>
            <li>复制 kimi-auth 这一项的 Value 粘贴到下方</li>
          </ol>
          <textarea
            className="input textarea"
            rows={3}
            placeholder="粘贴 kimi-auth 的值，支持整串 cookie 自动识别"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            spellCheck={false}
            autoComplete="off"
          />
          {errMsg !== null && <p className="hint-err">{errMsg}</p>}
          {okMsg !== null && <p className="hint-ok">{okMsg}</p>}
          <div className="row-end">
            {configured && (
              <button
                type="button"
                className="btn danger"
                onClick={() => void clear()}
                disabled={busy}
              >
                {clearing ? "清除中…" : "清除"}
              </button>
            )}
            <button
              type="button"
              className="btn primary"
              onClick={() => void save()}
              disabled={busy || input.trim() === ""}
            >
              {saving ? "校验中…" : "校验并保存"}
            </button>
          </div>
        </>
      )}
    </section>
  );
}
