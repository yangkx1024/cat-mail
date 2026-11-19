# 同步按钮改用 macOS 式菊花转，Junk 换图标

- 日期：2026-11-19
- 类型：功能

## 背景

同步时 Sync 按钮只是把箭头图标原地旋转（连同按钮的悬停底色一起转），不像 Mac 上的加载状态；按钮同时被禁用，
变暗到 0.4。侧栏 Junk 用的 `shield-exclamation`（盾牌加感叹号）更像安全警告，不像垃圾邮件。

## 决策

- nsh 没有原生的 spinner，自己画：一张 SVG（`assets/spinner.svg`）上 12 根圆头辐条，透明度从 1 线性降到 0.25，
  用 nsh 的 `Easing::Steps(12, JumpEnd)` 每次跳 30°，与 AppKit 的 `NSProgressIndicator` 一样是一根一根地走，
  而不是连续旋转。辐条用 `currentColor`，取 muted 前景色。
- 同步中按钮不再变暗：菊花转本身说明了状态；只在没有账户时变暗。悬停底色与点击仍只在可用时有。
  不再"转完当前一圈才停"：菊花转停下时直接换回图标，没有要回正的角度。
- Junk 换成 Heroicons 的 `no-symbol`（圆圈加斜杠，Outlook 的意思）。考虑过 `archive-box-x-mark`（接近 Apple
  Mail），但它和 Archive 的盒子图标太像。

## 方案

- `assets/spinner.svg`、`icons.rs` 的 `Icon::Spinner`。
- `buttons.rs`：`icon_button` 拆出 `button_box`（框、悬停、按下、提示），新增 `busy_icon_button`：忙时图标换成
  `spinner`（动画随 spinner 显示开始、随它消失结束），不可点但不变暗。`mailbox/list/mod.rs` 的 `sync_button`
  只是调用它，去掉 `finish_turn`。
- `assets/heroicons/{outline,solid}/no-symbol.svg` 取代 `shield-exclamation.svg`，`icons.rs` 里 Junk 对应它。
- 文档：`docs/architecture/gui.md` 的同步一段。clippy、GUI 测试通过；效果要在运行的 App 里看。
