import { useState } from "react";
import { useTranslation } from "react-i18next";
import { clearWebToken, openExternalUrl, setWebToken } from "../ipc";

/** Kimi 网页版地址（步骤引导里引导用户登录） */
const KIMI_WEB_URL = "https://www.kimi.com";

interface WebTokenSectionProps {
  /** 目标账号 id（网页凭证按账号隔离） */
  accountId: string;
  /** 后端是否已配置网页 token（CredentialStatus.web_token_configured） */
  configured: boolean;
  /** 保存/清除成功后回调，父组件重新拉取凭证状态 */
  onChanged: () => void;
  /** bare=true 时去掉 scard 折叠外壳（内嵌账号卡使用），直接渲染表单体 */
  bare?: boolean;
}

/** 设置页"高级：月度总量"分区：网页 refresh_token 的粘贴校验、保存与清除（按账号配置）。
 *  新鉴权体系下粘贴一次 refresh_token 即可，插件会自动续期（见后端 kimi::web）。 */
export function WebTokenSection({ accountId, configured, onChanged, bare = false }: WebTokenSectionProps) {
  const { t } = useTranslation();
  // 默认收起，点击标题行展开/收起（bare 模式无折叠，恒展开）
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
      setErrMsg(t("webToken.errEmpty"));
      setOkMsg(null);
      return;
    }
    setSaving(true);
    setErrMsg(null);
    setOkMsg(null);
    try {
      const info = await setWebToken(accountId, token);
      setInput("");
      setOkMsg(
        t("webToken.saved", {
          total: info.total_pct.toFixed(1),
          kimi: info.kimi_pct.toFixed(1),
          code: info.code_pct.toFixed(1),
        }),
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
      await clearWebToken(accountId);
      setOkMsg(t("webToken.cleared"));
      onChanged();
    } catch (e) {
      setErrMsg(String(e));
    } finally {
      setClearing(false);
    }
  };

  const body = (
    <>
      <p className="hint-muted">{t("webToken.hint")}</p>
      <ol className="steps">
        <li>
          {t("webToken.step1pre")}{" "}
          <button
            type="button"
            className="link"
            onClick={() => void openExternalUrl(KIMI_WEB_URL)}
          >
            https://www.kimi.com
          </button>{" "}
          {t("webToken.step1post")}
        </li>
        <li>{t("webToken.step2")}</li>
        <li>{t("webToken.step3")}</li>
      </ol>
      <textarea
        className="input textarea"
        rows={3}
        placeholder={t("webToken.placeholder")}
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
            {clearing ? t("webToken.clearing") : t("webToken.clear")}
          </button>
        )}
        <button
          type="button"
          className="btn primary"
          onClick={() => void save()}
          disabled={busy || input.trim() === ""}
        >
          {saving ? t("webToken.saving") : t("webToken.save")}
        </button>
      </div>
    </>
  );

  // bare 模式：无折叠外壳（嵌入账号卡的凭证配置区）
  if (bare) {
    return (
      <div className="login-body">
        <p className="sub-title">
          {t("webToken.title")}
          {configured && <span className="badge">{t("webToken.configured")}</span>}
        </p>
        {body}
      </div>
    );
  }

  return (
    <section className="scard">
      <button
        type="button"
        className="collapse-head"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
      >
        <span className="scard-title">{t("webToken.title")}</span>
        {configured && <span className="badge">{t("webToken.configured")}</span>}
        <span className={`chevron${open ? " open" : ""}`}>▸</span>
      </button>
      {open && body}
    </section>
  );
}
