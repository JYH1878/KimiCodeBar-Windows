# 隐私政策 / Privacy Policy

## 一句话结论

KimiCodeBar 不收集、不上传任何统计或遥测数据。你的所有凭证与用量数据只存在于这台电脑上。

## 数据存哪里（全部本机）

| 数据 | 位置 |
|---|---|
| API Key（Kimi / DeepSeek / GLM 主 Key + 额外 Key） | Windows 凭据管理器（槽位 `api_key/<账号id>` / `api_key_extra/<账号id>`） |
| OAuth 登录凭证 | `%APPDATA%\KimiCodeBar\` 下 DPAPI 加密文件（仅当前 Windows 用户可解） |
| 网页 refresh_token / kimi-auth | Windows 凭据管理器 |
| 设置、用量缓存、本地历史、本地消耗统计（scan-state） | `%APPDATA%\KimiCodeBar\` |
| 运行日志 | `%APPDATA%\KimiCodeBar\logs\`（按天滚动，不含任何凭证明文） |

删除账号时，该账号的全部本地数据（凭据槽位、OAuth 文件、缓存、历史）一并清除；卸载应用即删除全部数据。

## 网络请求只发往这些地方

- `api.kimi.com` — Kimi 用量查询
- `auth.kimi.com` — OAuth 授权 / token 续期
- `api.deepseek.com` — DeepSeek 余额（仅当配置了 DeepSeek 账号）
- `open.bigmodel.cn` — GLM Coding Plan 额度（仅当配置了 GLM 账号）
- `api.github.com` / `github.com` — 更新检查（版本号比对）与打开 Releases 下载页

没有中间商、没有统计埋点、没有行为分析。应用内不会主动发起上述之外的任何网络连接。

## 本地消耗统计（v1.6.0 起）

- 只扫描本机文件：Kimi Code CLI 的 `~/.kimi-code*` 会话日志，以及 Claude Code / Codex / OpenCode 的本地日志（OpenCode 的 SQLite 数据库只读打开）
- 归属用的 API Key 只在内存中参与比对，**不落盘、不联网**（额外 Key 同样只用于本地归属，绝不参与任何网络请求）
- 统计结果（按账号 / 日期的 token 数）存本机 scan-state.json，不上传

## 诊断导出

设置页的「诊断与日志」导出由你手动触发：内容是脱敏后的配置状态、运行日志与最近一次 API 响应样例，不含任何 Key / token 明文。导出文件只在你本机生成，是否分享由你决定。

---

## English Summary

KimiCodeBar collects no telemetry and no usage statistics. All credentials and data stay on this machine (Windows Credential Manager + `%APPDATA%\KimiCodeBar\`). Network requests go only to: `api.kimi.com` (usage), `auth.kimi.com` (authorization), `api.deepseek.com` (DeepSeek balance, only if a DeepSeek account is configured), `open.bigmodel.cn` (GLM quota, only if a GLM account is configured) and `api.github.com` (update checks). Local token statistics scan only local log files; API keys are matched in memory only — never persisted, never sent anywhere. Diagnostics export is manual and sanitized. See the Chinese sections above for details.
