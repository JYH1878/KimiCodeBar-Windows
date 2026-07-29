# 贡献指南

感谢您愿意为 KimiCodeBar for Windows 出一份力。本项目是社区驱动的开源工具（MIT 协议），任何形式的贡献都欢迎：bug 报告、功能建议、文档改进、代码 PR。

## 提 Issue

- **Bug**：使用 Bug 报告模板，尽量附复现步骤与诊断文件（应用内：设置 → 诊断与日志 → 导出诊断文件，内容不含任何密钥）
- **功能建议**：使用功能请求模板，先讲使用场景和痛点，再讲方案
- **安全相关**（凭证泄露风险等）：请不要公开提 Issue，先通过 GitHub 私信联系维护者

## 本地开发

前置：[Rust](https://rustup.rs/)（1.77+）、[Node.js](https://nodejs.org/)（18+）

```bash
git clone https://github.com/JYH1878/KimiCodeBar-Windows.git
cd KimiCodeBar-Windows
npm install

npm run tauri dev     # 开发模式（Vite 热更新 + 托盘应用）
```

## 提交前必过的检查（CI 同样会跑）

```bash
npm run lint        # ESLint（前端）
npm run build       # tsc 类型检查 + vite 构建

cd src-tauri
cargo fmt --check   # Rust 格式
cargo clippy --all-targets -- -D warnings   # Rust lint
cargo test          # 单元/集成测试
```

改动 Rust 代码请同步补充/更新测试（解析类逻辑必须有 fixture 测试）；改动前端请保证 `npm run build` 零警告。

## Commit 与 PR 规范

- Commit message 使用 [Conventional Commits](https://www.conventionalcommits.org/zh-hans/)：`feat:` / `fix:` / `docs:` / `chore:` / `refactor:` / `test:`，正文写中文
- PR 标题同格式；描述里说明"解决了什么"和"如何验证"
- 一个 PR 只做一件事；大改动请先到 Issue 里讨论方案

## 发版流程（仅维护者）

`npm run bump -- X.Y.Z` 一键同步四处版本号（含 Cargo.lock）→ 提交推送 → `git tag vX.Y.Z && git push origin vX.Y.Z` → CI 自动构建并在 Releases 生成草稿 → 人工检查发布 → Publish 后运行 `npm run bump-bucket -- X.Y.Z` 更新 [scoop-bucket](https://github.com/JYH1878/scoop-bucket) 清单（winget 上架后还需同步更新 `packaging/winget/` 清单并给 winget-pkgs 提 PR）。
