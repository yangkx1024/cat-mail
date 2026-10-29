# 修复深色转换对 CSS 变量和带转义颜色的处理

- 日期：2026-10-29
- 类型：修复

## 背景

Codex review 指出深色转换（`recolor_html`）的问题，用临时测试都复现了：

- 自定义属性（CSS 变量）一律当边框反转。`--fg:white;color:var(--fg);background:#222` 变成
  `--fg:#202020`，深色字配深色底，看不见。
- 带转义的颜色不转换。`color:bl\61 ck` 原文交给 `color` crate，它不认转义，于是不动；写回时
  cssparser 又解码成 `black`，结果深色页面上一段黑字。

## 决策

- 先试过按用法给变量定用途：转换前扫一遍样式表和 `style` 属性，看 `var(--x)` 用在什么属性上。
  第二轮 review 又找出三种反例：一个变量用于多种用途（`--fg:white` 同时用于文字和边框，冲突时反转又
  成了深字配深底）、变量互相引用（`--fg:var(--base)`，用途没传到 `--base`）、同名变量在不同规则里
  用途不同被合并。变量在使用处的值由层叠和继承决定，只有引擎知道；在源码上怎么猜都还会有下一个
  反例。换规则（比如冲突时文字优先）也只是把错误换个地方。
- 所以改为：颜色用了变量的邮件不转换，按原样以浅色显示（和 Apple Mail 的做法一致）。保证可读，代码
  也比按用法分析少。代价是这类邮件在深色主题下是白底；主流邮件客户端（Gmail、Outlook）不支持
  `var()`，邮件模板很少用。
- "转换不了"交给渲染：`Appearance::recolor` 钩子返回 `None` 时，文档线程改用默认外观打开（白画布、
  不加读信区样式表、`prefers-color-scheme` 为 light）。判断仍在文档线程上，不占 UI 线程，GUI 不用
  事先知道。
- 带转义的颜色：先按现有的记号写回逻辑写成解码后的文本再交给 `color` crate，不截取原文。

## 方案

- `crates/catmail-html/src/css.rs`：值里有颜色要转换的属性写到 `var(` 时记下（`Css::variables`，像
  `dark` 一样向上合并为 `Stylesheet::colors_from_variables`）；自定义属性本身的值不转换；`parse_color`
  用 `copy` 写出解码文本；`recolor_style` 返回 `Stylesheet`。
- `sanitize.rs`：`recolor_html` 返回 `Option<String>`，样式表或任一 `style` 属性的颜色用了变量时为
  `None`；`styles` 在深色转换时也收集 `style` 属性。
- `catmail-render`：`Recolor` 返回 `Option<String>`，`worker.rs` 的 `recolored` 在文档线程上调用它，
  `None` 时改用 `Appearance::default()`。
- 文档：`docs/architecture/bodies.md`、`gui.md`。
- 验证：css.rs `moves_escaped_colors`、`finds_colors_set_with_custom_properties`；sanitize.rs 样式表和
  属性里用变量时返回 `None`；render 测试钩子拒绝时按浅色画（白画布、浅色样式）；GUI 测试
  `shows_html_dark_in_a_dark_theme` 用变量设色的邮件在深色主题下显示白底（改动前是深色）。
