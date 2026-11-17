# 回复和转发里，原信的 HTML 不能盖住框外的内容

- 日期：2026-11-17
- 类型：修复

## 背景

#330（来自 #319 的全仓库审查）。原信 HTML 原样接在回复、转发后面（`catmail-proto` 的 `mime::build`），样式只做了
作用域。清洗（`catmail-html` 的 `css.rs`）只拒绝会取资源的函数，不过滤属性，所以
`<div style="position:fixed;inset:0;background:#fff;z-index:9999">` 能留下来。收件人的客户端只要认
`position`（Apple Mail、Thunderbird），转发出去的信里原信就能盖住转发者自己写的字和生成的头部表格，伪造
From / Date。阅读区不受影响：那里邮件就是整页。写信窗口的编辑器也不受影响，ProseMirror 按 schema 重新解析，
`style` 进不去。

## 决策

- 原因是原信放进别人的文档时没有边界，`position: fixed` 只是其中一种写法：`position: relative` 加 `top`、
  负外边距、`transform` 同样能把内容挪到框外。所以在发信这一侧给原信加一个裁剪的框，而不是在清洗时拦属性。
- 框是 `.catmail-original` 外面的一层 div：`position:relative;overflow:auto;contain:paint`。
  `contain: paint` 让 `fixed` 以它为参照并在这里裁掉；不认 `contain` 的客户端靠 `position: relative` 和
  `overflow` 挡住其他写法。用 `auto` 不用 `hidden`，比收件人窗口宽的原信还能横向滚动。
- 不直接给 `.catmail-original` 加样式：作用域会把原信的 `body` 改写成 `.catmail-original`，原信的样式表能用
  `margin-top` / `transform` 把整个元素挪走，或者用 `!important` 盖掉框的样式；外层 div 没有 class，改写后的
  选择器选不中它（兄弟选择器和 `:has()` 本来就拒绝）。
- 放弃的方案：在 `css.rs` 丢掉 `position: fixed|absolute|sticky`（Gmail 的做法）。它只堵一种写法，负外边距、
  `transform`、`relative` 加偏移照样能盖住；还会改变阅读区的显示，要升 `CONTENT_VERSION` 重建已存的正文。
  也没另在引文路径上去掉 `position` 兜底：框已经裁住这些写法，没必要多加一遍清洗。
- 框里面的内容是原信自己的，它在框里画一张假的头部表格挡不住，也不该由 CSS 管。

## 方案

- `crates/catmail-proto/src/mime/build.rs`：新增常量 `CONTAINED`，`quote_html` 把 `.catmail-original` 包在里面。
  只改发出的 HTML；已存的草稿引文是原信的快照，发出时才包，也一并生效。
- 测试：`a_quote_is_kept_from_covering_what_is_around_it`（`position: fixed` 的 `style` 属性，样式表用 `body`
  的负外边距和 `transform`），原有的两条引文测试改为期望外层框。去掉包层后这三条都失败。
- `docs/architecture/sending.md` 的"怎么发出"写明这层框和原因。
- 手动检查：用 headless Chrome 渲染三种写法（`fixed` 全屏覆盖、`relative` 加 `top` 负值、样式表给 `body` 负外边距），
  修复前都盖住了转发头部，修复后都留在原信自己的框里。没在 WebKit（Apple Mail）里看。
