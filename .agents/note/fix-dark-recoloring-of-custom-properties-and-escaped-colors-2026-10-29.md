# 修复深色转换对自定义属性和带转义颜色的处理

- 日期：2026-10-29
- 类型：修复

## 背景

Codex review 指出深色转换（`recolor_html`）的两个问题，用临时测试都复现了：

- 自定义属性一律当边框反转。`--fg:white;color:var(--fg);background:#222` 变成
  `--fg:#202020`，深色字配深色底，看不见；`--bg:black;background:var(--bg);color:white` 变成浅色底
  配白字。变量的用途取决于使用它的属性，不取决于定义它的地方。
- 带转义的颜色不转换。`color:bl\61 ck` 原文交给 `color` crate，它不认转义，于是不动；写回时
  cssparser 又解码成 `black`，结果深色页面上一段黑字。

## 决策

- 自定义属性按用法决定用途：转换前把样式表和 `style` 属性先按同一套重写走一遍（不移动自定义属性），
  收集值里 `var(--x)` 所在属性的用途。只用于一种的按那种，多种的反转（`Role::Line`），没用在颜色上的
  不动。考虑过一律不动：`--text:#333` 用在已变深的背景上又成了深字配深底，同样有错。没有 `--` 的
  邮件跳过这一遍。
- 颜色先按现有的记号写回逻辑写成解码后的文本再交给 `color` crate，不截取原文。

## 方案

- `crates/catmail-html/src/css.rs`：`Recolor`（调色板加自定义属性的用途），`Sheet` / `Declarations`
  改为借用它并收集用法（`uses`，像 `dark` 一样向上合并），`custom_properties_used`；`parse_color`
  用 `copy` 写出解码文本。`dark.rs` 的 `Role::of` 不再处理 `--`。
- `sanitize.rs`：`styles` 在深色转换时也收集 `style` 属性，`recolor_html` 用它们建 `Recolor`。
- 文档：`docs/architecture/bodies.md`。
- 验证：css.rs `moves_escaped_colors`、`moves_custom_properties_as_their_uses_color`（修复前都失败），
  sanitize.rs 里属性中定义、样式表中使用的变量。
