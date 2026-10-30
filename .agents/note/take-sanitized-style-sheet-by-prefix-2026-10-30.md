# 读信区按前缀取清洗后的样式表

- 日期：2026-10-30
- 类型：重构

## 背景

issue #170：`recolor_html`（深色主题每次显示）和 `clean`（`embed_inline_images`）的输入都是
`sanitize_html` 的输出，却仍用 `style_sheets` 把整封信分词一遍找 `<style>`，随后 ammonia 再完整解析一遍。
release 下量了生成的营销邮件（每行带 `style` / `bgcolor` 的表格、300 条规则）：53 KB 时分词 251 µs，
占 `recolor_html` 的 18%；193 KB / 586 KB 时 1.4 / 4.5 ms，约占 25%。按前缀取约 2 µs，与信件大小无关。

## 决策

- 清洗的输出里样式表只可能是开头 `with_sheet` 写的那个不带属性的 `<style>`：其它 `<style>` 都被 ammonia
  去掉，`css::sanitize` 的输出里没有 `<`。所以直接取开头 `<style>` 到第一个 `</style>`。
- 正确性因此依赖"输入是 `sanitize_html` 的原样输出"，写进文档并加测试钉住。不满足时只会丢样式表，
  不影响安全。
- 考虑过 `clean` 不重新着色时跳过 `css::sanitize`、原样沿用已清洗的样式表：只有"在浏览器中打开"走这条
  路，收益小，还要多依赖一条"重写幂等"的约定，没做。

## 方案

- `crates/catmail-html/src/sanitize.rs`：新增 `sanitized_sheet`；`recolor_html` 和 `clean` 改用它；
  `sanitize_html` 处理原始邮件，不再经 `clean`，直接 `style_sheets` + `with_sheet`。
- 测试 `the_sanitized_sheet_is_the_one_tokenizing_finds`：多张表、带 `media`、写在正文后面的、`\r`、NUL、
  SVG / `textarea` 里的、没有样式表的，清洗后两种取法结果一致；`a_style_sheet_cannot_end_its_element`
  的变异 XSS 输入也加上同一断言，并改用 `sanitized_sheet` 检查样式表里没有 `<`。
- `docs/architecture/bodies.md` 写明读信区的几遍处理只接受清洗的原样输出。
