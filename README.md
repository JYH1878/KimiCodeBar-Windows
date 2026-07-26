# KimiCodeBar for Windows

[![GitHub release](https://img.shields.io/github/v/release/JYH1878/KimiCodeBar-Windows)](https://github.com/JYH1878/KimiCodeBar-Windows/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Windows%2010%20%2F%2011-blue)](https://github.com/JYH1878/KimiCodeBar-Windows)

Kimi Code 用量监控的 Windows 系统托盘工具。常驻托盘不打扰，左键一点看额度，快烧完了变红提醒你。

基于 **Tauri 2 + Rust + React** 构建：安装包不到 3 MB，运行内存约 40–80 MB。

> 社区版本，非 Kimi 官方出品。

## 功能特性

- **系统托盘常驻**：左键弹出用量面板（自动定位到托盘图标上方，失焦收起）；右键菜单（刷新 / 设置 / 退出）
- **双窗口用量**：7 天窗口 + 5 小时窗口的已用百分比、剩余量、重置倒计时
- **会员与钱包**：显示会员档位（Andante / Moderato / Allegretto / Allegro）与 Booster 钱包余额、月度用量
- **双模式登录**
  - **方式A · API Key**：手动输入 `sk-kimi-` 前缀的 Key，存入 Windows 凭据管理器
  - **方式B · 账号授权**：OAuth 设备码流程（与 Kimi Code CLI 相同），浏览器一键授权，自动续期
- **自动刷新**：默认每 5 分钟轮询（1–60 分钟可调）
- **低额度预警**：任一窗口剩余低于阈值（默认 20%，可调）时托盘图标变红，并推送系统通知（可关闭）
- **离线可用**：最近一次查询结果本地缓存，断网时照常展示，不报错不崩溃
- **自动更新**：静默检查 GitHub Releases，有新版本时面板出现更新徽标，点击直达下载页
- **开机自启**：可选，设置里一键开关

## 界面预览

![用量面板](docs/screenshots/panel.png)

![设置页](docs/screenshots/settings.png)

## 安装

**方式一 · 安装包（推荐）**：到 [Releases](https://github.com/JYH1878/KimiCodeBar-Windows/releases) 下载 `KimiCodeBar_x.x.x_x64-setup.exe`，双击安装——当前用户安装，**无需管理员权限**，装完出现在开始菜单。

**方式二 · 便携版**：下载 `KimiCodeBar_x.x.x_x64-portable.zip`，解压到任意目录直接运行 `kimicodebar.exe`，不写注册表。注意配置与缓存仍存于 `%APPDATA%\KimiCodeBar\`（如需随目录携带，可设置环境变量 `KIMICODEBAR_CONFIG_DIR` 指向自定义目录）。

首次启动后图标常驻系统托盘（可能在"隐藏的图标"折叠区，拖出来即可）。

系统要求：Windows 10 1809+ / Windows 11；依赖 WebView2 运行时（Windows 11 已预装，缺失时安装包会引导安装）。

## 使用指南

### 首次配置（二选一）

**方式A · API Key**
1. 打开 [kimi.com/code/console](https://www.kimi.com/code/console) 创建 API Key（以 `sk-kimi-` 开头）
2. 托盘右键 → 设置 → 选择"方式A"→ 粘贴 Key → 保存

**方式B · 账号授权**
1. 托盘右键 → 设置 → 选择"方式B"→ 点击"开始授权登录"
2. 点击"打开浏览器授权"，在网页里确认授权码
3. 授权成功后自动登录，token 到期自动续期

### 日常使用

- **看用量**：左键单击托盘图标
- **手动刷新**：面板上的刷新按钮，或托盘右键 → 刷新
- **调设置**：刷新间隔、告警阈值、告警开关、开机自启均在设置页

## 常见问题

**`sk-kimi-` 的 Key 和开放平台的 `sk-` Key 是一回事吗？**
不是。Kimi Code（kimi.com/code/console）的 Key 以 `sk-kimi-` 开头；开放平台（platform.moonshot.cn）的 `sk-` Key 用于通用大模型 API，两者不互通。本工具只接受 `sk-kimi-` Key（设置页会校验并提示）。

**会员档位 Andante / Moderato / Allegretto / Allegro 是什么？**
Kimi 会员等级的官方命名（音乐速度记号，由慢到快对应由低到高）。工具按 API 返回原样显示。

**Booster 是什么？**
Kimi Code 的按量付费钱包：订阅额度用完后，可用预存余额继续按量使用。未开通时卡片显示"未开通"；开通后显示余额与月度已用/限额。

**断网了会怎样？**
面板照常展示最近一次成功查询的缓存数据，顶部出现提示横幅，不会报错或崩溃；网络恢复后自动刷新。

**我的数据会被上传到别处吗？**
不会。所有凭证与配置仅存本机（API Key 在 Windows 凭据管理器，其余在 `%APPDATA%\KimiCodeBar\`）。网络请求只发往：`api.kimi.com`（用量）、`auth.kimi.com`（授权）、`api.github.com`（更新检查）。

## 从源码构建

前置：[Rust](https://rustup.rs/)（1.77+）、[Node.js](https://nodejs.org/)（18+）

```bash
git clone https://github.com/JYH1878/KimiCodeBar-Windows.git
cd KimiCodeBar-Windows
npm install

npm run tauri dev     # 开发模式（热更新）
npx tauri build       # 产出 NSIS 安装包与便携 exe（src-tauri/target/release/）
```

运行测试：

```bash
cd src-tauri && cargo test
```

## 技术栈

| 层 | 选型 |
|---|---|
| 框架 | Tauri 2（系统托盘、无边框面板、通知、自启、 opener） |
| 后端 | Rust：reqwest（native-tls）、serde、tokio、keyring（Windows 凭据管理器） |
| 前端 | React 18 + TypeScript + Vite，无重型 UI 依赖 |
| 持久化 | JSON 文件（settings / cache）+ Windows 凭据管理器（API Key）+ 本地凭证文件（OAuth） |
| 打包 | NSIS（当前用户安装，免管理员） |

## 致谢与相关项目

本项目的行为语义与 API 细节（设备码流程、用量响应解析的边界情况）参考了以下项目：

- [xifandev/KimiCodeBar](https://github.com/xifandev/KimiCodeBar) — macOS 原版（MIT 协议），本项目的灵感来源；作者也在开发官方 WinUI 3 Windows 版，技术路线与本项目不同
- [Golden0Voyager/kimi-code-usage](https://github.com/Golden0Voyager/kimi-code-usage) — Kimi Code 用量 CLI（Python）参考

## License

[MIT](LICENSE) © 2026 xifandev, JYH1878
