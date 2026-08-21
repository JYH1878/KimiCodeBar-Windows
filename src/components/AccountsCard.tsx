import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { Account, AccountProvider, CredentialStatus, LoginMethod } from "../types";
import {
  addAccount,
  deleteAccount,
  getCredentialStatus,
  listAccounts,
  moveAccount,
  renameAccount,
  setAccountLoginMethod,
  setApiKey,
  setWebToken,
} from "../ipc";
import { ApiKeySection } from "./ApiKeySection";
import { DeviceLoginSection } from "./DeviceLoginSection";
import { WebTokenSection } from "./WebTokenSection";

interface AccountsCardProps {
  /** 折叠态（首装无账号时父组件强制展开引导） */
  open: boolean;
  onToggle: () => void;
  /** 定位信号：递增表示收到 settings-navigate("account-add")，
   *  卡片滚动到添加表单并聚焦名称输入框 */
  addFocusTick: number;
}

/** 设置页「账号」管理卡：账号列表（改名/排序/二次确认删除）+ 添加表单 +
 *  按账号的凭证配置区（登录方式 / API Key / OAuth / 网页 token 全部从全局迁入本卡） */
export function AccountsCard({ open, onToggle, addFocusTick }: AccountsCardProps) {
  const { t } = useTranslation();
  const [accounts, setAccounts] = useState<Account[]>([]);
  // 各账号的凭证状态徽标（按账号 id 索引）
  const [statusMap, setStatusMap] = useState<Record<string, CredentialStatus>>({});
  // 展开凭证配置区的账号 id；null = 全部收起
  const [expandedId, setExpandedId] = useState<string | null>(null);
  // 改名进行中的账号 id 与输入值
  const [renaming, setRenaming] = useState<{ id: string; value: string } | null>(null);
  // 二次确认删除：第一次点删除记下 id，行内出现确认/取消
  const [deletingId, setDeletingId] = useState<string | null>(null);
  const [rowError, setRowError] = useState<string | null>(null);

  // ---- 添加表单 ----
  const [newName, setNewName] = useState("");
  const [newProvider, setNewProvider] = useState<AccountProvider>("kimi");
  const [newMethod, setNewMethod] = useState<LoginMethod>("api_key");
  const [newKey, setNewKey] = useState("");
  const [newWebToken, setNewWebToken] = useState("");
  const [creating, setCreating] = useState(false);
  const [addError, setAddError] = useState<string | null>(null);
  const [addOk, setAddOk] = useState<string | null>(null);
  // 新建 OAuth 账号后自动发起设备码授权（绑定新账号；一次性）
  const [autoStartOAuthFor, setAutoStartOAuthFor] = useState<string | null>(null);

  const addFormRef = useRef<HTMLDivElement>(null);
  const nameInputRef = useRef<HTMLInputElement>(null);

  /** 重拉账号列表与全部凭证徽标（≤10 个账号，一次全拉开销可忽略） */
  const reload = useCallback(async () => {
    try {
      const list = await listAccounts();
      setAccounts(list);
      const entries = await Promise.all(
        list.map(async (a) => {
          try {
            return [a.id, await getCredentialStatus(a.id)] as const;
          } catch {
            return null;
          }
        }),
      );
      const map: Record<string, CredentialStatus> = {};
      for (const e of entries) if (e !== null) map[e[0]] = e[1];
      setStatusMap(map);
    } catch {
      // 列表拉取失败不打断设置页，下次操作时再试
    }
  }, []);

  /** 单账号凭证状态重拉（该账号凭证变更后调用） */
  const reloadStatus = useCallback(async (accountId: string) => {
    try {
      const status = await getCredentialStatus(accountId);
      setStatusMap((m) => ({ ...m, [accountId]: status }));
    } catch {
      // 失败不打断，下次操作时再试
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  // 面板「+」定位到添加表单：滚动 + 聚焦名称输入
  useEffect(() => {
    if (addFocusTick === 0) return;
    addFormRef.current?.scrollIntoView({ behavior: "smooth", block: "start" });
    nameInputRef.current?.focus();
  }, [addFocusTick]);

  /** 改名提交（失焦/回车）：空名后端报错原样展示 */
  const commitRename = async () => {
    if (renaming === null) return;
    const { id, value } = renaming;
    setRenaming(null);
    setRowError(null);
    try {
      await renameAccount(id, value);
      await reload();
    } catch (e) {
      setRowError(String(e));
    }
  };

  const move = async (id: string, direction: number) => {
    setRowError(null);
    try {
      await moveAccount(id, direction);
      await reload();
    } catch (e) {
      setRowError(String(e));
    }
  };

  /** 二次确认的第二步：真正删除（该账号凭证/缓存/历史由后端一并清除） */
  const confirmDelete = async (id: string) => {
    setDeletingId(null);
    setRowError(null);
    try {
      await deleteAccount(id);
      if (expandedId === id) setExpandedId(null);
      await reload();
    } catch (e) {
      setRowError(String(e));
    }
  };

  /** 切换某账号的登录方式（立即持久化，该账号下次刷新生效） */
  const switchMethod = async (id: string, method: LoginMethod) => {
    setRowError(null);
    try {
      await setAccountLoginMethod(id, method);
      await reload();
    } catch (e) {
      setRowError(String(e));
    }
  };

  /** 添加账号：建账号 → 存登录方式 → 按需存 API Key / 网页 token。
   *  DeepSeek / GLM 账号只有 API Key 一种凭证（固定 login_method=api_key，无网页 token）。
   *  账号建好后凭证保存失败不回滚账号（列表照常刷新，错误原样展示，可在配置区补配） */
  const create = async () => {
    setCreating(true);
    setAddError(null);
    setAddOk(null);
    let account: Account;
    try {
      account = await addAccount(newName.trim() === "" ? undefined : newName.trim(), newProvider);
      await setAccountLoginMethod(account.id, newProvider === "kimi" ? newMethod : "api_key");
    } catch (e) {
      setAddError(String(e));
      setCreating(false);
      return;
    }
    // 方式A 且填了 Key：随创建一并保存（不填则创建后在该账号配置区再配）；
    // 可选的月度总量 refresh_token：填了就校验保存（在线校验失败抛中文错误，仅 Kimi 账号）
    let credError: string | null = null;
    try {
      if ((newProvider !== "kimi" || newMethod === "api_key") && newKey.trim() !== "") {
        await setApiKey(account.id, newKey.trim());
      }
      if (newProvider === "kimi" && newWebToken.trim() !== "") {
        await setWebToken(account.id, newWebToken.trim());
      }
    } catch (e) {
      credError = String(e);
    }
    setNewName("");
    setNewKey("");
    setNewWebToken("");
    setExpandedId(account.id);
    // OAuth 账号：连贯引导，自动发起设备码授权
    setAutoStartOAuthFor(newProvider === "kimi" && newMethod === "oauth" ? account.id : null);
    await reload();
    if (credError !== null) {
      setAddError(t("accounts.createdButCredFailed", { name: account.name, error: credError }));
    } else {
      setAddOk(t("accounts.created", { name: account.name }));
    }
    setCreating(false);
  };

  const atCap = accounts.length >= 10;

  return (
    <section className="scard">
      <button type="button" className="collapse-head" onClick={onToggle} aria-expanded={open}>
        <span className="scard-title">{t("accounts.title")}</span>
        <span className="muted-text">{t("accounts.count", { count: accounts.length })}</span>
        <span className={`chevron${open ? " open" : ""}`}>▸</span>
      </button>
      {open && (
        <>
          {rowError !== null && <p className="hint-err">{rowError}</p>}
          {accounts.length === 0 && <p className="hint-muted">{t("accounts.empty")}</p>}

          {/* 账号列表：行内改名 / 上移下移 / 二次确认删除；点击行头展开凭证配置区 */}
          {accounts.map((a, i) => {
            const status = statusMap[a.id];
            const expanded = expandedId === a.id;
            return (
              <div key={a.id} className={`account-row${expanded ? " expanded" : ""}`}>
                <div className="account-head" onClick={() => setExpandedId(expanded ? null : a.id)}>
                  {renaming?.id === a.id ? (
                    <input
                      className="input grow"
                      value={renaming.value}
                      autoFocus
                      onClick={(e) => e.stopPropagation()}
                      onChange={(e) => setRenaming({ id: a.id, value: e.target.value })}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") void commitRename();
                        if (e.key === "Escape") setRenaming(null);
                      }}
                      onBlur={() => void commitRename()}
                    />
                  ) : (
                    <span className="account-name">{a.name}</span>
                  )}
                  {a.provider === "deepseek" && <span className="badge">DeepSeek</span>}
                  {a.provider === "glm" && <span className="badge">GLM</span>}
                  {status?.api_key_configured && <span className="badge">Key</span>}
                  {status?.oauth_configured && <span className="badge">OAuth</span>}
                  {status?.web_token_configured && <span className="badge">{t("accounts.monthlyBadge")}</span>}
                  <span className="account-actions" onClick={(e) => e.stopPropagation()}>
                    {deletingId === a.id ? (
                      <>
                        <span className="hint-err">{t("accounts.deleteConfirm", { name: a.name })}</span>
                        <button type="button" className="btn danger" onClick={() => void confirmDelete(a.id)}>
                          {t("accounts.confirmDelete")}
                        </button>
                        <button type="button" className="btn" onClick={() => setDeletingId(null)}>
                          {t("accounts.cancel")}
                        </button>
                      </>
                    ) : (
                      <>
                        <button
                          type="button"
                          className="btn icon-btn"
                          title={t("accounts.rename")}
                          onClick={() => setRenaming({ id: a.id, value: a.name })}
                        >
                          ✎
                        </button>
                        <button
                          type="button"
                          className="btn icon-btn"
                          title={t("accounts.moveUp")}
                          disabled={i === 0}
                          onClick={() => void move(a.id, -1)}
                        >
                          ↑
                        </button>
                        <button
                          type="button"
                          className="btn icon-btn"
                          title={t("accounts.moveDown")}
                          disabled={i === accounts.length - 1}
                          onClick={() => void move(a.id, 1)}
                        >
                          ↓
                        </button>
                        <button
                          type="button"
                          className="btn danger"
                          onClick={() => setDeletingId(a.id)}
                        >
                          {t("accounts.delete")}
                        </button>
                      </>
                    )}
                  </span>
                  <span className={`chevron${expanded ? " open" : ""}`}>▸</span>
                </div>

                {/* 凭证配置区（该账号）：Kimi = 登录方式单选 + 对应配置区 + 月度总量；
                    DeepSeek / GLM 只有 API Key 一种凭证，直接给 Key 配置区 */}
                {expanded && (
                  <div className="account-body">
                    {a.provider !== "kimi" ? (
                      <ApiKeySection
                        accountId={a.id}
                        provider={a.provider}
                        status={status ?? null}
                        onChanged={() => void reloadStatus(a.id)}
                      />
                    ) : (
                      <>
                        <label className={`radio-row${a.login_method !== "oauth" ? " active" : ""}`}>
                          <input
                            type="radio"
                            name={`login-method-${a.id}`}
                            checked={a.login_method !== "oauth"}
                            onChange={() => void switchMethod(a.id, "api_key")}
                          />
                          <span>{t("settings.loginMethod.apiKey")}</span>
                        </label>
                        <label className={`radio-row${a.login_method === "oauth" ? " active" : ""}`}>
                          <input
                            type="radio"
                            name={`login-method-${a.id}`}
                            checked={a.login_method === "oauth"}
                            onChange={() => void switchMethod(a.id, "oauth")}
                          />
                          <span>{t("settings.loginMethod.oauth")}</span>
                        </label>
                        {a.login_method === "oauth" ? (
                          <DeviceLoginSection
                            accountId={a.id}
                            oauthConfigured={status?.oauth_configured ?? false}
                            onChanged={() => void reloadStatus(a.id)}
                            autoStart={autoStartOAuthFor === a.id}
                          />
                        ) : (
                          <ApiKeySection
                            accountId={a.id}
                            status={status ?? null}
                            onChanged={() => void reloadStatus(a.id)}
                          />
                        )}
                        <WebTokenSection
                          accountId={a.id}
                          configured={status?.web_token_configured ?? false}
                          onChanged={() => void reloadStatus(a.id)}
                          bare
                        />
                      </>
                    )}
                  </div>
                )}
              </div>
            );
          })}

          {/* 添加表单（面板「+」定位锚点 id="account-add"） */}
          <div className="account-add" id="account-add" ref={addFormRef}>
            <p className="sub-title">{t("accounts.addTitle")}</p>
            {atCap ? (
              <p className="hint-muted">{t("accounts.maxReached")}</p>
            ) : (
              <>
                <input
                  className="input"
                  ref={nameInputRef}
                  placeholder={t("accounts.namePlaceholder")}
                  value={newName}
                  onChange={(e) => setNewName(e.target.value)}
                />
                {/* 提供商单选（默认 Kimi）：DeepSeek / GLM 只有 API Key，选后隐藏登录方式与网页 token */}
                <div className="add-method-row">
                  <label className={`radio-row${newProvider === "kimi" ? " active" : ""}`}>
                    <input
                      type="radio"
                      name="add-provider"
                      checked={newProvider === "kimi"}
                      onChange={() => setNewProvider("kimi")}
                    />
                    <span>Kimi</span>
                  </label>
                  <label className={`radio-row${newProvider === "deepseek" ? " active" : ""}`}>
                    <input
                      type="radio"
                      name="add-provider"
                      checked={newProvider === "deepseek"}
                      onChange={() => setNewProvider("deepseek")}
                    />
                    <span>DeepSeek</span>
                  </label>
                  <label className={`radio-row${newProvider === "glm" ? " active" : ""}`}>
                    <input
                      type="radio"
                      name="add-provider"
                      checked={newProvider === "glm"}
                      onChange={() => setNewProvider("glm")}
                    />
                    <span>GLM</span>
                  </label>
                </div>
                {newProvider === "kimi" && (
                  <div className="add-method-row">
                    <label className={`radio-row${newMethod === "api_key" ? " active" : ""}`}>
                      <input
                        type="radio"
                        name="add-login-method"
                        checked={newMethod === "api_key"}
                        onChange={() => setNewMethod("api_key")}
                      />
                      <span>{t("settings.loginMethod.apiKey")}</span>
                    </label>
                    <label className={`radio-row${newMethod === "oauth" ? " active" : ""}`}>
                      <input
                        type="radio"
                        name="add-login-method"
                        checked={newMethod === "oauth"}
                        onChange={() => setNewMethod("oauth")}
                      />
                      <span>{t("settings.loginMethod.oauth")}</span>
                    </label>
                  </div>
                )}
                {newProvider !== "kimi" || newMethod === "api_key" ? (
                  <input
                    className="input"
                    type="password"
                    placeholder={
                      newProvider === "deepseek" ? "sk-…" : newProvider === "glm" ? t("accounts.glmKeyPlaceholder") : "sk-kimi-…"
                    }
                    value={newKey}
                    onChange={(e) => setNewKey(e.target.value)}
                    spellCheck={false}
                    autoComplete="off"
                  />
                ) : (
                  <p className="hint-muted">{t("accounts.oauthHint")}</p>
                )}
                {newProvider === "kimi" && (
                  <textarea
                    className="input textarea"
                    rows={2}
                    placeholder={t("accounts.webTokenPlaceholder")}
                    value={newWebToken}
                    onChange={(e) => setNewWebToken(e.target.value)}
                    spellCheck={false}
                    autoComplete="off"
                  />
                )}
                {addError !== null && <p className="hint-err">{addError}</p>}
                {addOk !== null && <p className="hint-ok">{addOk}</p>}
                <div className="row-end">
                  <button type="button" className="btn primary" onClick={() => void create()} disabled={creating}>
                    {creating ? t("accounts.creating") : t("accounts.create")}
                  </button>
                </div>
              </>
            )}
          </div>
        </>
      )}
    </section>
  );
}
