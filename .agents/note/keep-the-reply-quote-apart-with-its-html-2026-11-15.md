# 回复和转发的引文与正文分开存，带上原信的 HTML 和图片

- 日期：2026-11-15
- 类型：功能

## 背景

issue #303：回复时引擎把原信的纯文本按行加 `> ` 写进正文，草稿 `html: None`，写信窗口再把 `>` 开头的行猜回引用。
原信是 HTML 时格式全丢（加粗、链接、表格、内嵌图片），引用结构两边各写一遍规则，用户自己写的 `>` 也会被当成引用。
转发同样只带纯文本，并特意不带内嵌图片。

## 决策

- **引文不进编辑器**：nsh RichEditor 是 ProseMirror 式的 schema，放不下一封邮件的 HTML。参考移动端（Gmail、Outlook 的
  app）的做法，引文单独存一份快照（`DraftContent::quote`），发信时接在正文下面。放弃了"按 schema 保留"（丢格式）
  和"编辑器里不可编辑的节点"（要自定义节点和序列化）。
- **存快照，不存原信 id**：原信可能被删或 expunge。存成 `drafts.quote_kind` / `quote_intro` / `quote_text` /
  `quote_html` 几列（未发布，直接改 `0001_init.sql`），不是一列 JSON：列表只读文字那几列。
- **列表不读 HTML**：`list_drafts` 改返回 `DraftSummary`（纯文本、`formatted`、引文的纯文本，没有正文和引文的
  HTML）。GUI 草稿列表每次 `DraftsChanged`（每次保存）都重读，原来每份草稿连同整封原信的 HTML 一起读出、留在
  每一行里。GUI 列表行和右栏、CLI 的 `draft list` 和 JSON、MCP 的 `DraftOut` 都改从摘要构造，单份草稿的输出用
  `DraftSummary::from(&Draft)`，同一套输出代码。
- **拼接在 proto**：`Outgoing::quote`，纯文本用 `Quote::below`（types，CLI / MCP 的输出也用它），HTML 在
  `mime::build`：回复用 `<blockquote type="cite">`，转发不缩进。正文没有 HTML 而引文有时，正文一行一段写成 HTML；
  两边都没有 HTML 时只发纯文本。
- **原信的样式表限定在引文里**：`catmail_html::nested` 把选择器改写到 `.catmail-original` 之内（`html` / `body` /
  `:root` 就是这个元素），放进文档头。编辑器的样式（`pre-wrap`、段落无边距）改为只作用于 `.catmail-text`，不再是
  全局的 `body` / `p`，否则会改掉原信的排版。
- **`>` 只在原信是纯文本时读作引用**（RFC 3676 的层级），在 proto 里一处。frontend 的 `text_to_html` 留给第二步。
- **图片**：原信 HTML 显示的内嵌图片复制成草稿的内嵌附件，换新 Content-ID（`catmail_html::rename_cid_images`）。
  取不到或超出附件上限的跳过，回复照样建出来；转发的普通附件取不到仍然整个失败。远程图片保留 URL。
  调用方给的文件先复制，图片用剩下的额度（review 指出：反过来时单独放得下的文件会因图片先占了额度而失败）；
  额度按复制时定下的身份算，草稿也用它建。
- **图片清理**：不是编辑器做的修改（CLI、MCP）当场删掉正文和引文都不显示的图片；编辑器做的仍等关窗
  （`drop_unshown_images`），撤销能把引文连同图片带回来。原来的判断是"正文不是 HTML 就全删"，会删掉引文的图片。
- **编辑器保存不带引文**：`DraftChanges::all` 不动引文，写信窗口的模型也不持有它；否则每次自动保存都要克隆、比较、
  序列化并重写整封原信的 HTML。去掉引文、撤销去掉都显式给 `DraftChanges::quote`（第二步）。
- 写信窗口还不显示引文（第二步）；草稿栏的预览显示发出时的纯文本。
- HTML 转义合并成 `catmail_html::escape`，proto 和 frontend 共用。

## 方案

- `catmail-types`：`Quote`、`QuoteKind`、`DraftContent::quote`、`text_as_sent`、`DraftChanges::quote`。
- `catmail-types`：`DraftSummary`；`catmail-store`：`drafts.quote_*` 列、`Drafts::list` 只读摘要的列（`SUMMARY_COLUMNS`）。
- `catmail-html`：`css::sanitize_scoped`（选择器限定）、`nested`、`rename_cid_images`。
- `catmail-proto`：`Outgoing::quote`，`html()` / `quote_html()` / `quoted_paragraphs()`，`html_document` 带上引文的样式表。
- `catmail-core`：`reply_content` / `forward_content` 生成引文，`quoted_images` 复制图片，`shown_images` 统一正文和引文。
- CLI：`draft edit --no-quote`，`--json` 的 `quote`，`draft show` 显示发出时的纯文本。MCP：输出 `quote`、`update_draft`
  的 `remove_quote`，`reply` 的下载预算算上引文的图片。
- 文档：`sending.md` 新增"引文"一节，`mcp.md`、`cli.md`、`gui.md` 随之更新。
- 测试：样式表限定（含 `html > body`）、`nested`、改写 `cid:`；MIME（HTML 原信整块引用且样式不外泄、纯文本原信只发纯文本 / 在 HTML 下
  `>` 转嵌套引用、转发不缩进）；引擎端到端（转发带图片和附件、去掉引文时图片的清理、取不到的引文图片被跳过、
  回复发出的纯文本带引文、要附上的文件优先于引文图片）；store 往返；types 的纯文本拼接。
