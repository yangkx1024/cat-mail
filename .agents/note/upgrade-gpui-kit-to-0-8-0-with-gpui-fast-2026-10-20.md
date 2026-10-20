# 升级 gpui-kit 到 0.8.0 并改用 GPUI Fast 后端

- 日期：2026-10-20
- 类型：重构

## 背景

gpui-kit 0.8.0 发布，"Editor and text input" 一节修了输入框和多行文本框的若干问题：单行视口里光标被裁切、
重排和高度变化时光标不可见、纵向滚动越界、自动增高的 textarea 滚动条拖不到底、点击时光标闪掉等。
HTML 渲染也修了 `<br>`：原先它是单独的块，前后文字仍挤在一行；现在是段落内换行，对读信区直接有益。
同时开启 `gpui-fast` feature，把后端从 gpui-pre 换成带渲染和布局优化的 GPUI Fast（0.1.10）。

## 决策

- 两项都不用改代码：GUI 只用了 `Input`、`Textarea`、`TextView`，不涉及 0.8.0 的破坏性变更
  （`SelectionFormat`、`TabVariant`、`ResizeHandle`、`ScrollBounce`、`set_line_number` 等都没用到）；
  GPUI Fast 由 gpui-kit 在同样的导入路径下导出。
- 没有用新加的 `input_action` / `autocorrect` / `autocapitalize`：它们是给软键盘的提示，GPUI 在
  Linux（Wayland text-input）上把内容类型写死为 `None` / `Normal`，桌面端设了也没效果。
- 0.8.0 起"可滚动的文本区在到头后也不把滚动交给外层"：写信窗口的正文 textarea 占满剩余高度、不在
  滚动容器里，不受影响。macOS 上默认的回弹滚动行为随之改变，目前只发 Linux，接受默认。
- 代价：feature 是叠加的，gpui-pre 仍会编译，还多出一份 wgpu 30（gpui-pre 用 29），冷编译变慢。
  GPUI Fast 的 `cosmic_text_system.rs` 与 gpui-pre 相同，可变字体字重的上游问题照旧。

## 方案

- 根 `Cargo.toml`：`gpui-kit = { version = "0.8.0", features = ["gpui-fast"] }`，`Cargo.lock` 随之更新；
  `cocoa`、`unicode-properties` 仍满足 `gpui-fast-*` 的精确 pin。
- 对照 0.8.0 的 gpui-base `text/format/html.rs`：块元素表和 `parse_table_cell`（单元格仍只排行内内容）
  都没变，`unwrap_layout_tables` 的规则继续成立。
- `docs/architecture/gui.md`：说明后端是 GPUI Fast，更新已知问题里引用的版本和依赖锁定一条。
- 内置的 gpui-kit skills 与上游 `v0.8.0` 标签逐字节相同，只更新 `crates/catmail-gui/.claude/skills/README.md`
  里记录的 commit。
- 验证：`cargo fmt`、`clippy -D warnings`、全量测试通过；跑了 `screenshots`（ignored 测试）在 GPUI Fast
  上离屏渲染，读信区（HTML 邮件、表格、链接、列表）和写信窗口显示正常。macOS / Windows 需在 CI 上跑。
