# 升级 nsh，用上 macOS 隐藏标题栏的高度修复

- 日期：2026-11-07
- 类型：重构

## 背景

longbridge/nsh#1068：macOS 上 `set_title_bar(Hidden, 48 pt)` 时 nsh 装上 `Unified` 的空工具栏，
`title_bar().top` 报 66 pt，而红绿灯居中在约 52 pt 的一行里。`chrome.rs` 因此只用 `title_bar()` 判断有没有
红绿灯，高度写死 52 pt（`WINDOW_BUTTONS_ROW`）。上游已由 longbridge/nsh#1196 修复（我们的 #1198 未合并）：
不再装工具栏（macOS 26 起带工具栏的窗口圆角更大），nsh 自己把红绿灯居中在应用要的高度里，`title_bar().top`
就是这个高度。

## 决策

- nsh 升到 main（`0a9fa7b`），而不是只到 #1196 的 `e12c6f8`：和以前一样跟 main，#1196 依赖的每个窗口的
  `WindowRef`（#1137）也在里面。
- 主窗口直接向 nsh 要 52 pt（之前要 48 pt、靠 `Unified` 工具栏得到 52 pt 的按钮行），外观不变；各栏顶部的高度
  改从 `title_bar().top` 读，常量只剩下要的高度。
- 写信窗口仍要 34 pt。之前 `UnifiedCompact` 工具栏最少 40 pt，现在真是 34 pt，和 GPUI Kit 的标题栏一样高。
- 两种窗口的圆角都变成普通标题栏窗口的（不再有工具栏）。

## 方案

- `Cargo.toml`、`Cargo.lock`：nsh 四个 crate 换到 `0a9fa7b1859d2e24bc7b829385b37692182f6dc3`。
- nsh 去掉了 `set_title_bar`、`set_title`，改成 `current_window().set(WindowAttributes)`。`chrome.rs` 加
  `hide_title_bar`（`TitleBarOptions` 是 `non_exhaustive`，从 `default()` 改字段）和 `follow_title`；
  `composer/view.rs` 用 `chrome::follow_title` 设窗口标题。
- `crates/catmail-gui/.claude/skills/` 换成 `0a9fa7b` 的版本（README 记下 commit）。
- `docs/architecture/gui.md`、`crates/catmail-gui/ROADMAP.md` 去掉 #1068 的 workaround 说明。
- 验证：`cargo fmt`、`clippy -D warnings`、`cargo test --workspace`。这台 Mac 没有 Nix，直接用了 cargo。
  macOS 上要手动看：主窗口红绿灯在 52 pt 顶行的中间、与工具栏对齐；全屏后顶行回到 48 pt；写信窗口标题栏 34 pt、
  标题居中、窗口标题跟随 Subject。
