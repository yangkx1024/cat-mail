# 升级 nsh 到 0.7.1，去掉动画期间隐藏滚动指示器的绕路

- 日期：2026-11-26
- 类型：重构

## 背景

nsh 发布 0.7.1，带上了我们报的 longbridge/nsh#3188（由 #3192 修复）：macOS 上滚动视图的 frame 大小变化时
（含布局动画的每一帧）不重新定位滚动指示器。我们为它在右栏开关、侧栏折叠时把列表和侧栏的指示器关掉一段时间
（`motion::hide_indicators`），见 `animate-sidebar-folds-and-list-rows-2026-11-25.md` 的追加一节，当时约定修了就去掉。

## 决策

- 整个去掉 `hide_indicators`、`indicators_shown` 和列表、侧栏的 `.scrollbar(...)`，回到 nsh 的默认。
  - #3192 在 `NshView::place` 里视图大小变化时再调 `update_overlays`：右栏开关时列表的边在动，指示器每帧跟着走。
  - 侧栏折叠（`Collapsible` 的 reveal）动的是滚动视图里面的区域，滚动视图本身大小不变，内容高度在提交时一次
    变到终点，指示器的长度也随之一次变好，不跟着弹簧走。右栏开关时行变高也一样。这是 #3188 里说过、接受的
    "内容大小总是一次变好"，所有滚动视图都这样，不再为它单独隐藏。
- `motion::reveal_spring()` 只是为了让 `hide_indicators` 知道折叠要多久才显式写出来，和 nsh-base 的默认相同，
  一起去掉；`fold` 用 `Collapsible` 的默认弹簧。`gui.md`「升级 nsh 时核对」里对应的两条也去掉。
- longbridge/nsh#3235（`list::hold` 的纯偏移）已关闭，但修复 #3361 在 0.7.1 之后才合并，绕路保留。
- 0.7.0 → 0.7.1 其余变化（canvas、Windows、Linux 声音改为运行时加载 GStreamer、CLI 等）不碰我们；
  `Cargo.lock` 只有 nsh 的九个 crate 换版本，没有新依赖。

## 方案

- `Cargo.toml`、`Cargo.lock`：六个 nsh crate 到 0.7.1。
- `crates/catmail-gui/src/motion.rs`、`mailbox/sidebar.rs`、`mailbox/list/mod.rs`：去掉隐藏指示器。
- `docs/architecture/gui.md`、`crates/catmail-gui/ROADMAP.md`：去掉 #3188 的 ⚠️ 和核对项。
- `crates/catmail-gui/.claude/skills/`：换成 v0.7.1 的 skills（`f78eace`）。
- 验证：macOS 上 `cargo fmt`、`clippy -D warnings`、`cargo test --workspace`、`cargo deny check`。
  指示器跟着动画走要在 Mac 上手动看：悬停在列表上打开、关上右栏，悬停在侧栏上折叠、展开账户。
