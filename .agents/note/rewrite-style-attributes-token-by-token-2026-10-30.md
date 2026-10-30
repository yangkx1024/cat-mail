# style 属性统一逐记号重写

- 日期：2026-10-30
- 类型：重构

## 背景

issue #162：行内 `style` 属性有两套规则。`sanitize_html`（入库）原样保留，只用 `css::fetches`
判断会不会取资源，会的话整个属性去掉；`recolor_html`（深色）每个 `style` 都经 `Declarations`
逐记号重写。同一封信浅色和深色下经过的规则不同，安全论证也要分两条路径，以后容易越走越远。

## 决策

- `sanitize_html` 也一律用 `Declarations` 重写 `style`（`css::sanitize_style`，取代
  `recolor_style`，带可选的调色板）。浅色和深色只有一种规则，深色只是同一遍里多换颜色。
- 删掉 `css::fetches`：浏览器读到的是重写的输出，不是原文，原文经浏览器错误恢复会留下什么不必
  再判断（它连解析失败的部分也看，就是为原样写回准备的）。和样式表一致，取资源的声明只丢那一条，
  重写后什么都不剩才去掉整个属性。
- 空值的自定义属性（`--gap: ;`）以前被当作坏声明丢掉（样式表里一直如此），但它合法，且和没设
  不同：丢掉后 `var(--gap,1000px)` 会用上后备值，布局大变。现在保留，写成 `--gap: ;`（留一个空格，
  兼容还要求至少一个记号的旧引擎）；Codex review 指出。
- `url (…)`（名字和括号之间有空白）按语法不是函数，不再特别拒绝，保留无害。
- 顺带把 IE 的 `expression()` 加进拒绝的函数，样式表和属性都丢；现在没有引擎执行它，丢掉不影响显示。
- 已入库的正文保持旧规则清洗的结果，只有之后下载的正文按新规则，两种会并存一段时间；两种都能被
  `recolor_html` 正确处理。

## 方案

- `crates/catmail-html/src/css.rs`：`sanitize_style`，删 `fetches` 及其测试；拒绝的函数列表加
  `expression` 后不再只是"取资源"，改名 `REFUSED` / `is_refused`。`sanitize_style` 返回只有
  `css` 和 `colors_from_variables` 的 `Style`（属性的 `adapts_to_dark` 从来没人读），
  `Declarations::new` 取代两处结构体更新写法。
- `crates/catmail-html/src/sanitize.rs`：`attribute_filter` 对 `style` 统一走 `sanitize_style`，
  `recolor_attribute` 不再处理 `style`。
- `docs/architecture/bodies.md` 更新清洗规则。
- 测试 `keeps_empty_custom_properties`（没有修复时失败）；`rewrites_style_attributes_as_declarations`：`!important`、厂商前缀、`mso-*` 保留；取资源、
  `expression()`、解析不了的声明只丢那一条；`sanitize_html` 幂等；属性值里的引号和 `<script>`
  在浅色和深色下都只是属性值。
