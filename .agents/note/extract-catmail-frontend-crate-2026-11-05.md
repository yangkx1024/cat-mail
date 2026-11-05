# 抽出前端共用的 catmail-frontend

- 日期：2026-11-05
- 类型：重构

## 背景

`nsh-spike` 分支上的 `catmail-nsh` 要逐项对齐 `catmail-gui`。gpui 版里有一批不依赖窗口的逻辑，两个前端都要用，而且行为必须一致：读写同一份 `settings.json`；用同一个 socket 协议做单实例，后启动的那个能唤起先启动的，不管各自是哪个前端；显示格式；读信区显示哪些图片。这些代码原先是 catmail-gui 的私有模块，别的前端用不了。

## 决策

- **新建 `catmail-frontend`**，没有放进 `catmail-core`：设置文件、主题名这些是前端的偏好，不属于引擎；core 的边界保持只有 `MailClient`。分层是 `core ← frontend ← gui`。
- **按需要逐步抽**：这次只抽 nsh 骨架（P1）马上要用的 `instance`、`settings`（数据和文件读写）、`format`、`html`。`folders`、`refresh` 各有一处耦合（`Hero` 图标、`message_list::Source`），等 P2、P3 用到时再拆。`theme` 的推导结果是 gpui-kit 的 `ThemeConfig`，留到 P7。
- **GUI 侧保持原路径**：main.rs 里写 `use catmail_frontend::{format, html, instance};`，`crate::format` 这类路径照旧能用，其余代码不用改。`Preferences` 仍是 GPUI 的 Global，只是改为持有 `SettingsFile`；写入仍交给 GPUI 的后台 executor。
- **`instance` 的错误从 anyhow 改成 thiserror 的 `HandError`**：库里用 thiserror（AGENTS.md）。GUI 打印时转成 `anyhow::Error`，`{:#}` 照样带上原因。

## 方案

- `git mv` 了 `format.rs`、`html.rs`、`instance.rs`；新写 `settings.rs`（`Settings`、`Appearance`、`TEXT_SIZES`，以及 `SettingsFile` 的 load、queue、write_pending、themes_dir 和 `write_atomically`）、`theme.rs`（`DEFAULT_LIGHT`、`DEFAULT_DARK`）。
- catmail-gui 去掉了不再用到的 `data-url`、`dunce`、`serde`、`uds_windows`。
- 更新了 AGENTS.md 的 crate 表和分层，以及 `docs/architecture.md`、`architecture/gui.md`、`architecture/background.md`。
- 验证：原有的单测随模块一起迁移（instance、format、html、settings 共 17 个，在 catmail-frontend 下通过）；workspace 的 fmt、clippy `-D warnings` 和全量测试都通过。
