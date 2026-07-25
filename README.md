# KimiCodeBar for Windows

Kimi Code 用量监控的 Windows 系统托盘版：左键弹面板看额度，右键菜单刷新/设置/退出，低额度自动变红提醒。

> **声明**：本项目灵感与 API 逻辑参考自 [xifandev/KimiCodeBar](https://github.com/xifandev/KimiCodeBar)（MIT 协议，macOS 版）。这是一个基于 **Tauri 2 + Rust + React** 的**独立实现**，不是官方版本。官方也在开发自己的 [WinUI 3 Windows 版](https://github.com/xifandev/KimiCodeBar/tree/main/Windows)。

## 功能

- **系统托盘常驻**：左键弹出用量面板（定位在托盘图标上方，失焦自动收起），右键菜单（刷新/设置/退出）
- **用量展示**：7 天窗口与 5 小时窗口的已用/剩余、重置倒计时、会员档位（Andante / Moderato / Allegretto / Allegro）、Booster 钱包余额
- **双模式登录**
  - 方式A：手动输入 API Key（`sk-kimi-` 前缀，[在 kimi.com/code/console 获取](https://www.kimi.com/code/console)；注意与开放平台 platform.moonshot.cn 的 `sk-` Key 不通用）
  - 方式B：OAuth 设备码授权（与 Kimi Code CLI 同一流程），支持 refresh_token 自动续期
- **自动刷新**：默认每 5 分钟轮询（可调）；任一窗口剩余低于阈值（默认 20%）时托盘图标变红并推送系统通知
- **离线缓存**：配置与最近一次查询结果存本地，断网时展示缓存数据，不报错崩溃
- **自动更新**：启动后检查 GitHub Releases，有新版本面板显示更新徽标，一键跳转下载
- **开机自启**：可选（写入 `HKCU\...\Run`，随设置保存即时生效）

## 安装

到 [Releases](https://github.com/JYH1878/KimiCodeBar-Windows/releases) 下载最新安装包（NSIS，当前用户安装，无需管理员权限）。

系统要求：Windows 10 1809+ / Windows 11，依赖 [WebView2 运行时](https://developer.microsoft.com/microsoft-edge/webview2/)（Windows 11 与大多数 Windows 10 已预装）。

## 从源码构建

前置：[Rust](https://rustup.rs/)（1.77+）、[Node.js](https://nodejs.org/)（18+）

```bash
npm install
npm run tauri dev        # 开发模式（热更新）
npx tauri build          # 产出 NSIS 安装包与独立 exe（src-tauri/target/release/）
```

测试：

```bash
cd src-tauri && cargo test
```

## 隐私与安全

- API Key 存于 **Windows 凭据管理器**；OAuth 凭证存于 `%APPDATA%\KimiCodeBar\credentials.json`
- 配置与缓存位于 `%APPDATA%\KimiCodeBar\`（`settings.json` / `cache.json`）
- 网络请求只发往三个地址：`api.kimi.com`（用量查询）、`auth.kimi.com`（OAuth 授权）、`api.github.com`（更新检查），不向任何第三方上传数据

## 致谢

- [xifandev/KimiCodeBar](https://github.com/xifandev/KimiCodeBar) — macOS 原版，本项目的行为语义与 API 细节（设备码流程、用量解析边界）均以其源码为参考，MIT 协议

## License

[MIT](LICENSE) © 2026 xifandev, JYH1878
