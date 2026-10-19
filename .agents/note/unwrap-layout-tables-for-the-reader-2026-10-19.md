# 读信区展开布局表格

- 日期：2026-10-19
- 类型：功能

## 背景

M2 最小读信窗口的验证结论：gpui-kit 的表格单元格只是一个行内段落，营销邮件用
`<table width=600><tr><td>` 套住整个正文时，标题、段落、内层表格、列表全被压成一段（复现：
`crates/catmail-gui/fixtures/newsletter.eml` 切到 HTML）。PR #5 留下的下一步就是渲染前展开
布局表格，在那之前只能"在浏览器打开"。

## 决策

- **判定规则**：表格的某个单元格里有块元素，或每行至多一个单元格，就是布局表格；`table` 及其
  `tbody` / `tr` / `td` / `caption` 改成 `div`（属性保留），`colgroup` / `col` 丢弃。其余是数据表格，
  不动：单元格里只有行内内容，gpui-kit 能排。按"有块元素"判定而不是猜宽度、`role` 等：这正是
  gpui-kit 排不了的情况，`role="presentation"` 也已被清洗掉。gpui-kit 块元素表里缺的块元素
  （`center`、`dd`、`dt`、`hgroup`，对照 0.7.3）一律改 `div`：不只是 `center`（营销邮件用它包布局
  表格），定义列表、`hgroup` 里的标题也会被压平，按同一条规则处理而不是逐个加特例。
- **这是绕开上游限制**（gpui-base `parse_table_cell` 把单元格建成一个段落），升级 gpui-kit 时对照，
  单元格支持块元素后删掉表格部分。
- **放在 proto，按 DOM 处理，不在前端改文本**（架构文档原定）。ammonia 的
  `Document::to_dom_node` 需要 `--cfg ammonia_unstable`，不可用；`markup5ever_rcdom` 没有对应
  html5ever 0.40 的版本，用 gpui-base 的 0.27 + rcdom 会让 proto 跟着 GUI 的依赖走。最终直接依赖
  html5ever 0.40.1（ammonia 同版本，锁文件不增加 crate）：清洗后的 HTML 由 DOM 序列化而来、
  标签必然正确嵌套，tokenizer 的记号流就够，两遍（判定、改写）后用 html5ever 的 serializer 输出。
  结果再过一遍 sanitizer，防止这一步引入清洗不允许的内容。
- **只用于读信区**：浏览器能排表格，"在浏览器打开"仍用原样清洗的 HTML；存库的也是原样版本，
  以后换渲染器不用重建。展开在打开邮件时做，`OpenMessage::new` 整体挪到后台线程（它原本就要
  解析 HTML 找远程图片）。
- 多列布局会变成上下排列，对窄的读信区可以接受。
- **/simplify 整理**：没有可改内容时返回 `Cow::Borrowed`（先按字节找 `<table` 等，再看判定结果），
  多数非营销邮件不再多解析三遍，GUI 也让 `html` 与 `browser_html` 共用同一个字符串；改写只用一个
  开放元素栈和一个关闭函数（void 元素压栈后立即关闭）；文本记号直接保留 `StrTendril`。没采纳：把
  `has_blocked_images` 并进最后那次清洗（会把两个无关接口绑在一起，快速路径已覆盖多数邮件）。

## 方案

- `catmail-proto/src/mime/layout.rs`：`unwrap_layout_tables`；`catmail_core::html` 重新导出。
  根 `Cargo.toml` 加 `html5ever = "0.40.1"`。
- `catmail-gui/src/reader.rs`：`OpenMessage` 分 `html`（给 `TextView`，已展开）和
  `browser_html`（给浏览器页面）；`test_support::TestMail::download`。
- 文档：架构文档"GUI"一节、README 的规划说明。

验证：proto 5 个单测（布局表格变块且内层数据表格保持原样、单列表格与只含行内内容的表格、
文本与属性转义后不出现 `<script`、无表格的 HTML 原样借用、`hgroup` / `dt` / `dd` / `center` 变
`div`）；GUI 单测
`the_text_view_gets_layout_tables_unwrapped`（营销邮件经假服务器下载，读信区只剩 1 个表格、
浏览器版本仍是 3 个、远程图片仍能被发现）；GUI 不调用展开时它失败。离屏截图确认营销邮件的
标题、段落、数据表格、链接、列表分块显示。
