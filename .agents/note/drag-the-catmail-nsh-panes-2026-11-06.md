# nsh 版三栏可以横向拖动

- 日期：2026-11-06
- 类型：功能

## 背景

gpui 版的侧栏和列表可以拖动改宽（GPUI Kit 的 `h_resizable`），nsh 版是固定宽度。

## 决策

- 宽度和范围同 gpui 版：侧栏 240（192–384），读信时列表 368（256–640）。没有读信时列表铺满。宽度只在运行期间
  记着，gpui 版也不存。
- 先用了 nsh-base 的 `h_resizable`。快速来回拖几次后，它会丢掉一次拖动结束，之后分隔线和面板错位。所以宽度改由
  应用自己记（`PaneWidths`），只用它单独的 `resize_handle`。每次移动都从拖动开始时的宽度算起，丢了结束也不会
  累积。两边的宽度分开读，拖一边只重排它影响的部分；宽度到了边界不再写入。
- 拖动条照 GPUI Kit 的样子：1 px 的边线，悬停、按下时出现一根小把手。
- 读信区不再用 `invisible` 藏起来，而是画在列表下面（列表不透明）：
  - 原因是在亮色主题下从 Inbox 切到 Sent 再打开邮件，读信区会闪一帧黑。录屏逐帧看过，就是 web view 所在的
    那块。nsh 的 `invisible` 不画这个 view 和它的原生子 view，一阵没画的 web view 再画时，WebKit 画好之前
    会先出一帧黑。
  - 它也不能移出布局：nsh 只显示有尺寸的原生 view，WebKit 也只往显示着的 view 里加载页面。
  - 没读信时它固定宽 256，也就是列表最窄的宽度，所以拖列表边时不会白白去改一个看不见的 web view 的大小。

## 方案

`crates/catmail-nsh/src/app.rs`（`PaneWidths`、`SIDEBAR_WIDTHS`、`LIST_WIDTHS`、`edge_handle`、`panes`）。
验证：多次快速来回拖动后切换邮件，分隔线不再错位；录屏确认 Inbox 到 Sent 不再闪黑。catmail-nsh 的 clippy
`-D warnings` 和测试通过。
