# 极简模式（issue #27）进度记录

- 任务0 基线：`npm run lint && npm run build` 绿；`cd src-tauri && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test` 绿（基线 219 测试全过）。
- 契约同步 minimal_mode（5 处）：
  1. `src/types.ts` AppSettings 加 `minimal_mode: boolean`（autostart 之后，注释照现有风格）。
  2. `src-tauri/src/storage.rs` Settings 加 `#[serde(default)] pub minimal_mode: bool`，Default impl 给 false。
  3. `src-tauri/src/commands.rs` AppSettings DTO 加字段，两个 From impl 同步。
  4. `src/settings.tsx`：GeneralForm/初始态/加载回显/saveGeneral 组装各加字段，通用设置卡在 autostart 后加开关控件（照抄 autostart 写法）。
  5. `src/ipc.ts` mockDb.settings 加 `minimal_mode: false`。
- i18n：`src/i18n/zh.json` `"minimalMode": "极简模式（面板只显示 7 天 / 5 小时用量）"`；`src/i18n/en.json` `"minimalMode": "Minimal mode (panel shows only 7-day / 5-hour usage)"`。
- 面板 `src/panel.tsx`：PanelApp 加 `minimal` 状态（getSettings 首读 + onSettingsChanged 订阅，走 216 行附近既有链路）；AccountPage 加 `minimal` prop，为 true 时只渲染页头/EmptyState/加载态/错误横幅/7天·5小时 UsageCard，MonthlyCard/TrendCard/LocalUsageCard/总额卡/MembershipCard/BoosterCard 整组隐藏。底栏与翻页未动。
- 窗口压矮 `src-tauri/src/panel.rs`：新增 `MINIMAL_PANEL_HEIGHT = 420.0`（逻辑像素）与 `base_panel_height(config_height, minimal_mode)`；`fit_panel_to_screen` 读 `storage::load_settings().minimal_mode` 决定基准高，仍只用 conf 逻辑像素 + 托盘显示器 `monitor.scale_factor()`。新增 `refit_open_panel`（面板可见时按托盘 rect 重算尺寸并重定位），`commands.rs` save_settings 广播 settings-changed 后调用，面板开着时切开关即时生效。
- 紧凑高度最终值：**350 逻辑像素**（初版 420，领导视觉验收后先压 20% 到 336，再拍板回调到 350；常量 `MINIMAL_PANEL_HEIGHT`）。
- 新增测试：
  - storage.rs：`settings_minimal_mode_defaults_to_false`（默认值 false）、`settings_legacy_json_without_minimal_mode_loads`（无该字段旧 json 兼容加载）。
  - commands.rs：`app_settings_dto_roundtrip_covers_minimal_mode`（DTO 双向转换覆盖新字段，含 true/false 两向）。
  - panel.rs：`base_height_switches_on_minimal_mode`、`compact_height_fits_normal_screen`、`compact_height_still_clamped_on_short_screen`。
  - 另：storage.rs 既有 `settings_save_load_roundtrip` 的结构体字面量补 `minimal_mode: true`（不加编不过；同时覆盖 true 值落盘往返），未改任何既有断言。
- 修错记录（新增测试自身写错 1 轮，非实现 bug）：`compact_height_still_clamped_on_short_screen` 首版断言 600px 屏会压 420，实际可用 584 > 420 不压；改为 300px 屏（可用 284 < 420）后绿。
- 反向验证：临时把 `Settings::default()` 的 `minimal_mode` 改成 true，`cargo test --lib storage::` 中 `settings_minimal_mode_defaults_to_false` 如预期 FAILED（14 passed; 1 failed），记录后已改回 false。
- 踩坑：panel.rs 属二进制 crate（main.rs `mod panel;`），引用存储层要写 `kimicodebar::storage::` 而非 `crate::storage::`（lib 里才有 crate::storage）。
- 终验（全绿）：`npm run lint && npm run build` ✓；`cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test` ✓，测试 140+60+4+21+0 = **225 = 基线 219 + 新增 6**。
