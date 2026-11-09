# 可选中文字与阅读页使用主题的选区颜色

- 日期：2026-11-09
- 类型：修复

## 背景

`text(..).selectable(true)` 不指定颜色时，nsh 用系统的蓝色画选区，不随主题变；阅读页（WebView）同样是 WebKit 自带的蓝色。nsh-base 的组件用主题的 `selection`，于是同一个窗口里选区有两种颜色，暗色主题下尤其刺眼。

## 决策

- 统一用主题的 `selection`。nsh 画可选中文字的选区时，把给定的颜色再乘 30% 的不透明度（同 iOS），所以无法画得比 30% 更浓：`theme::selection()` 把主题的颜色压到不超过 30%，app 文字和阅读页都用这同一个值，两处看起来一致。
- 传给 nsh 的 tint 是 `selection()` 的 alpha 除以 0.3，画出来正好是 `selection()`。
- 加 `SelectableText::selectable_text()` 扩展方法替代 `.selectable(true)`，避免以后新加的可选中文字又忘了设颜色。
- 阅读页在页面样式里写 `::selection { background }`，只设背景、不改文字颜色；放在消息自身样式之前，邮件自己定义的 `::selection` 仍然优先。

## 方案

- `crates/catmail-gui/src/theme.rs`：`selection()`、`SelectableText`；测试 `selectable_texts_show_the_themes_selection` 检查主题自带的 30% 颜色原样画出、不透明的颜色被压到 30%。
- 各处 `.selectable(true)` 换成 `.selectable_text()`：composer、表单错误、草稿、发件箱、面板、设置、toast、阅读头部。
- `crates/catmail-frontend/src/html/page.rs`：`Colors` 加 `selection`，页面样式写 `::selection`；测试 `a_selection_on_the_page_is_highlighted_as_in_the_app`。`reader::colors` 按页面明暗取 `theme::selection()`。
