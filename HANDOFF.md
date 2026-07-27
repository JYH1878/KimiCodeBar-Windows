# KimiCodeBar-Windows 交接记录（HANDOFF）

> 写给完全不了解背景的新会话。读这一篇就能无缝接手。
> 最后更新：2026-07-27（v0.5.0 已发布）

## 1. 这个项目是什么

Kimi Code（月之暗面的 CLI 编程助手）用量监控的 **Windows 系统托盘工具**。左键弹用量面板、右键菜单（刷新/设置/退出）、低额度红色告警。

- **仓库**：https://github.com/JYH1878/KimiCodeBar-Windows （MIT，双版权 xifandev + JYH1878）
- **上游参考**：[xifandev/KimiCodeBar](https://github.com/xifandev/KimiCodeBar)（macOS 原版，MIT；我们参考其 API 语义后独立实现，README 有声明）。注意：**本地的 KimiCodeBar-Mac 参考目录已被用户删除**，需要对照原版时从 GitHub 重新克隆
- **技术栈**：Tauri 2（Rust 后端）+ React 18/TS/Vite 前端（WebView2）。Rust 1.97、Node 24
- **本地路径**：`D:\SoftWares\KimiCodeBar\KimiCodeBar-Windows`（git 仓库根；前端在 `src/`，Rust 在 `src-tauri/`）

## 2. 当前状态快照（截至本文时间）

**已发布**：v0.1.0 → v0.5.0（每版 changelog 见 Releases 页）。

**功能完全体**（全部实机验证过）：

- 托盘双键交互、无边框面板（托盘图标定位、失焦隐藏）
- 双窗口用量（7 天 / 5 小时，"已用%"语义，重置倒计时）、会员档位（Andante/Moderato/Allegretto/Allegro 原样显示）、Booster 钱包
- 双模式登录：API Key（sk-kimi- 前缀，存 Windows 凭据管理器）/ OAuth 设备码（RFC 8628，refresh_token 自动续期，凭证 DPAPI 加密存 `%APPDATA%\KimiCodeBar\credentials.json`）
- 月度总量（Kimi+Code 分列）：来自网页端 `GetSubscriptionStats`，需用户手动粘贴网页 cookie `kimi-auth`（OAuth token 与此接口不互通，已实测 401）
- 本地 token 消耗统计（扫描 `%USERPROFILE%\.kimi-code\sessions\**\wire.jsonl` 的 `usage.record` 事件，字节偏移增量扫描，3 分钟节流）
- 用量趋势（history.json 本地采样 7 天，纯 SVG 双线图；离线空档 >30 分钟断线）
- 自动更新（GitHub `releases/latest` 302 重定向查版本，绕开 API 限流；6h 成功缓存/10min 错误缓存；手动强制刷新）
- 中英双语（i18next 108 键 + Rust 查表）、浅/深色主题（CSS 变量 + matchMedia）
- 全局热键（默认禁用）、CLI `--status`（JSON 输出，退出码 0/1/2）、文件日志（tracing 按天滚动 7 天）、诊断导出（脱敏）、用量导出 CSV
- 单实例守护、开机自启（HKCU Run）、面板 800px 高

**工程体系**：CI 四门禁（fmt/clippy/ESLint/test）+ tag 触发自动构建（NSIS + 便携 zip → 草稿 Release）+ Dependabot 周更 + 分支保护（仅禁 force push/删除）+ 双 README（中英）。

## 3. 正在推进的任务（精确状态）

### 3.1 SignPath 开源签名 —— 等审核（约 1 周）

- 2026-07-26 通过 https://signpath.org 提交了 SignPath Foundation 开源签名申请（组织 KimiCodeBar，项目指到本仓库），审核结果发邮件
- **获批后要做**：① 用户把 API token 存到仓库 Secrets（`Settings → Secrets and variables → Actions`，名 `SIGNPATH_API_TOKEN`）；② 把 `signpath/github-action` 集成进 `.github/workflows/release.yml`（对 NSIS 产物签名）；③ README/Release 页补 "Code signing provided by SignPath Foundation"（申请条款要求）
- 集成后 SmartScreen 蓝色拦截消失，这是 v1.0.0 的门槛之一

### 3.2 网页 token 寿命观察 —— 被动等结果

- 月度总量用的 `kimi-auth` cookie 于 2026-07-26 上午粘贴
- 判定规则：月度卡报"网页登录态已过期"时记录间隔天数。**活 ≥7 天 → 功能转正（README 推）；活 <3 天 → 冷藏（保持可选不宣传）**
- 到现在（07-27 晚）尚未过期，暂时是好迹象

### 3.3 v1.0.0 的四个门槛

1. SignPath 签名集成（见 3.2）
2. cookie 寿命有结论（见 3.3）
3. 提交 winget（微软 winget-pkgs 提 PR）/ Scoop（自建 bucket json）分发
4. 用户自己的"天天在用没毛病"信心确认

## 4. 暂不做的（已否决，别再提）

多账号切换（存储层重做 vs 极小受众）、跨平台（macOS 有原版撞车、Linux 托盘雷区）、多供应商监控（zero-limit/aiusage 已占坑）、遥测/排行榜（与零上传隐私卖点冲突）、IDE 插件（另一个项目）、本地 HTTP API（攻击面）、E2E（托盘 UI 自动化 flaky）、插件机制（第二系统综合症）、cache.json 加密（无秘密）、用量预测（5 分钟粒度数据不足以建模，只做了"近 24h 趋势"纯事实 + 重置前 15 分钟提醒）、MSI（NSIS 够用）、卡片显隐开关（将来卡片再加才考虑）。

## 5. 本机环境坑（新会话必读，全是血泪）

1. **cargo 不在 PATH**：每条 Bash 先 `export PATH="$HOME/.cargo/bin:$PATH"`；cargo 一律加 `--offline`（依赖已全 vendored）。必须联网拉新依赖时：`CARGO_HTTP_PROXY="" GIT_CONFIG_GLOBAL=/dev/null CARGO_HTTP_MULTIPLEXING=false cargo fetch`，或走代理 `CARGO_HTTP_PROXY=http://127.0.0.1:7897`
2. **git 代理**：已配按域名分流 `http.https://github.com.proxy = http://127.0.0.1:7897`。梯子关了/抖动时 push 失败，用**直连兜底**：`git -c http.https://github.com.proxy= push`（实测有效）
3. **必须用 `npx tauri build --no-bundle -- --offline` 构建可运行 exe**。直接 `cargo build`（任何 profile）产出的 exe 内嵌 devUrl（localhost:1420），打开面板报"拒绝连接"——用户踩过两次。`target/debug/kimicodebar.exe` 是陷阱，别让任何人双击它
4. **NSIS 工具链本机下载超时**：Tauri 首次打包要下载 NSIS，GitHub 直连被重置。已用 `ghfast.top` 镜像手动部署到 `%LOCALAPPDATA%\tauri\NSIS`（含 `Plugins/x86-unicode|x86-ansi/nsis_tauri_utils.dll`）。CI 上无此问题
5. **shell 链陷阱**：`A && B > log 2>&1 & sleep N; C` 会把整条链后台化并吞掉输出，exe 可能根本没重建——**构建和启动必须拆成独立命令**，构建后 `ls -la` 核对 exe 时间戳再启动（踩过至少 5 次）
6. **Git Bash 里 Node 不认 `/c/` 路径**：`node -e` 读文件用 `process.env.APPDATA + '/KimiCodeBar/...'`
7. **杀进程再构建**：exe 被占用时 link 失败（os error 5），`taskkill //IM kimicodebar.exe //F` 先行
8. **reqwest 读 Windows 系统代理**（curl 不读）：网络抖动可能是进程级的（梯子 TUN/进程规则），别急着怀疑代码——先 curl 对照。GitHub 匿名 API 限流 60/h/IP（共享梯子出口更惨），应用内一律用 `releases/latest` 302 重定向查版本

## 6. 踩过的工程坑（修过的 bug，别再犯）

1. **keyring crate 裸声明无后端**：`keyring = "3"` 在 Windows 上是进程内 mock，重启即丢。必须 `features = ["windows-native"]`
2. **Tauri v2 的 `window.__TAURI__` 默认不存在**（需 `withGlobalTauri: true`）；探测运行环境要用 `__TAURI_INTERNALS__`——否则前端误判浏览器模式走 mock，面板曾经长期显示假数据（870/1000 那种）而无人察觉
3. **共享状态所有分支必须复位**：`do_refresh` 的 NoCredential 分支曾漏复位 `loading`，导致刷新按钮永久转圈、轮询全卡死。现在刷新是单航班合并（tokio Mutex）+ 45s 全局超时
4. **Tauri v2 ACL**：JS 调插件命令（opener 等）必须在 `src-tauri/capabilities/default.json` 声明权限，否则静默失败（"打开浏览器"按钮曾因此没反应）
5. **tauri-build 缓存嵌入图标**：换图标后要删 `target/*/build/kimicodebar-*` 强制重嵌，否则 exe 图标不更新
6. **Windows 图标缓存**：桌面快捷方式图标不随 exe 更新，`ie4uinit.exe -show` 或清 iconcache + 重启 Explorer
7. **版本错位**：存储格式迁移（如 DPAPI 加密）绝不能让开发版跑在发布版前面——用户旧版 exe 读新格式文件直接报错。发版先行，迁移殿后
8. **tauri-action 不显式传 tagName 会跳过 Release 创建**（"No releaseId or tagName provided"），`release.yml` 里用 `tagName: v__VERSION__` 钉死；改 tag 必须删掉重推（工作流以 tag 指向的 commit 为准）
9. **ESLint/框架类大版本升级**（eslint 10、react 19、TS 7）Dependabot 会开 PR 但 CI 必挂——绿勾小的合、红叉大的关、发版专用（tauri-action 等只被 release.yml 用的）暂缓到发版前实测

## 7. 常用操作手册

### 发版（唯一正确流程）

```bash
cd /d/SoftWares/KimiCodeBar/KimiCodeBar-Windows
npm run bump -- X.Y.Z          # 一键同步四处版本号（含 Cargo.lock）
git add -A && git commit -m "chore: bump version to X.Y.Z"
git push origin main            # 或直连兜底：git -c http.https://github.com.proxy= push origin main
git tag vX.Y.Z && git push origin vX.Y.Z
# CI 约 8-9 分钟构建 → Releases 页草稿 → 用户手动 Publish
```

- SemVer：修复=patch、新功能=minor、稳定宣言=major（计划 v1.0.0 见 §3.4）
- CI 有版本一致性检查：tag ≠ tauri.conf.json version 会构建失败
- 发布前用 FetchURL 查 `https://api.github.com/repos/JYH1878/KimiCodeBar-Windows/actions/runs?branch=vX.Y.Z` 确认 success（注意 API 限流，别狂刷）

### 本地验证命令

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cd src-tauri && cargo test --offline && cargo clippy --all-targets --offline -- -D warnings && cargo fmt --check
cd .. && npm run lint && npm run build
taskkill //IM kimicodebar.exe //F; npx tauri build --no-bundle -- --offline
./src-tauri/target/release/kimicodebar.exe   # 启动（托盘程序无输出即正常）
```

### 关键数据位置

- `%APPDATA%\KimiCodeBar\`：`settings.json`（设置）、`cache.json`（配额缓存）、`history.json`（趋势采样）、`credentials.json`（OAuth，DPAPI 加密）、`scan-state.json`（本地统计偏移）、`logs\kimicodebar.log.YYYY-MM-DD`（按天滚动 7 天）、`exports\`（导出的 CSV）
- Windows 凭据管理器（service `KimiCodeBar`）：`api_key`、`web_token`
- 注册表 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\KimiCodeBar`：自启

### 代码地图（src-tauri/src）

- `main.rs`：入口、插件注册、`cli::maybe_run()` 必须在第一行（先于单实例插件）
- `commands.rs`：命令层 + AppState + do_refresh 编排（单航班刷新）
- `kimi/`：client.rs（usages API）、oauth.rs（设备码 + DPAPI 凭证）、web.rs（月度 GetSubscriptionStats）、models.rs（wire 模型，全 Option 防御）、dpapi.rs
- `quota.rs`：解析 + "已用%"换算（Mac 原版是"已用"，我们保持，曾因用户要求从"剩余"改回）
- `history.rs` / `local_usage.rs` / `polling.rs` / `update.rs` / `storage.rs` / `creds.rs`（keyring）/ `logging.rs` / `diagnostics.rs` / `hotkey.rs` / `i18n.rs` / `tray.rs` / `panel.rs` / `cli.rs`
- 前端契约钉在 `src/types.ts`（改字段必须前后端同步）

## 8. 用户协作偏好（重要）

- 用户是项目 owner（GitHub: JYH1878），非专业程序员但学习极快；喜欢被如实告知"哪个是坑、哪个是臆想"，不喜欢盲目附和
- 验收驱动：每个功能必须实机可见才认；截图沟通很频繁
- 网络环境：梯子（Clash 系，127.0.0.1:7897）时开时关，GitHub 直连经常被重置——网络问题先怀疑环境再怀疑代码
- 发布按钮、GitHub 网页操作由用户亲手做；git 提交推送已授权助手直接执行
