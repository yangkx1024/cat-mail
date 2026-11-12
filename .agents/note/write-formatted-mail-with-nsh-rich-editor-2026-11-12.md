# 写信区换成 nsh 的 RichEditor，支持内嵌图片

- 日期：2026-11-12
- 类型：功能

## 背景

写信窗口的正文是一个多行输入框，草稿和发出去的信都只有纯文本。nsh 0.5.0 的 `nsh-base` 带了 `RichEditor`
（feature `rich-editor`）：ProseMirror 的 Rust 移植加 Tiptap 的扩展层，原生视图，支持 HTML 的读写、命令、粘贴文件事件，
图片节点能直接显示本地文件。

## 决策

- **草稿存两份正文**：`DraftContent::text`（纯文本）照旧，新增 `html: Option<String>`（`<body>` 的内容）。纯文本由
  编辑器从文档按"一段一行、引用加 `> `"写出，不用 html2text 从 HTML 转：后者段落之间空一行，用户敲一次回车，纯文本里就
  多一个空行。CLI、MCP、回复与转发的引文仍然只写纯文本，窗口打开时按行转成段落和引用。
- **只给 text 就清掉 html**（`DraftChanges::apply`）：agent 或 CLI 改了文字而旧 HTML 留着，会发出两份不同的内容。
  不另加"清空 HTML"的字段：HTML 只能和它的纯文本一起改。
- **图片是带 Content-ID 的草稿附件**（`draft_attachments.content_id`），HTML 用 `cid:` 引用。没选 data URI：编辑器
  每次保存都要序列化整篇文档，几 MB 的 base64 会跟着走一遍。编辑器里 `src` 是 blob 的文件路径
  （`attach_image` 一并返回，打开时 `DraftEditor::images` 一次取齐），存的时候在 ProseMirror JSON 上换回 `cid:`，不在 HTML 字符串上替换，避开属性转义。
- **发信只带 HTML 显示的图片**；从正文删掉的图片，附件行在关窗时才删（`DraftEditor::drop_unshown_images`）：编辑中
  保存就删的话，撤销把图片带回来时它已经没了内容。发送时不删，发送失败窗口和撤销都还在。为此去掉了
  `Closing::Now`：什么都存好了的关窗也经过 `Steps`，否则最常见的"自动保存后关窗"就漏掉了清理。只给纯文本、清掉 HTML
  的修改（agent、CLI）当场删掉全部内嵌图片，没有撤销的问题。
- **空白照写的保留**：打开草稿时按 `Whitespace::Full` 解析（默认会把缩进、连续空格压成一个，第一次编辑后就永久丢了，
  回复引文里的表格、代码最明显），发出的 HTML 加 `body { white-space: pre-wrap }`。
- **发出的段落和编辑器里一样**：编辑器里一段一行、段间没有间距，浏览器给 `<p>` 默认上下边距，收信人看到的每行都隔开；
  空段落（空行）没有高度、被吞掉。发出的 HTML 加 `p { margin: 0 }`，空段落写成 `<p><br></p>`（`fill_empty_paragraphs`，
  只在 proto 构建时做，草稿里存的不变）。
- **MIME 结构**自己用 `MimePart` 拼：mail-builder 的默认结构把内嵌图片放进 `multipart/mixed`，不是标准的
  `multipart/related`。
- 编辑器配置：starter kit 去掉任务列表（邮件里显示不了勾选框）。Mod-Enter 在 starter kit 里是换行，但 nsh 的平台键表
  先拿走窗口绑定的键（`keymap.rs`：platform 匹配后消费），Ctrl+Enter 仍是发送。
- 正文写出（JSON → HTML 和纯文本）按编辑器状态的 revision 缓存：只改了主题、收件人时自动保存不再重写正文。没有
  图片时直接序列化文档，不经 JSON 往返。
- 编辑器的命令会同步触发视图的 change 回调，所以 `Rich` 单独放在 `Rc` 里、内部用 `Cell` / `RefCell`，跑命令时不借用
  窗口的 `Composing`，避免 RefCell 重入 panic。

## 方案

- `catmail-types`：`DraftContent::html`、`DraftChanges::html`、`DraftAttachment::content_id`。
- `catmail-store`：`drafts.body_html`、`draft_attachments.content_id`（未发布，直接改 `0001_init.sql`）；
  `Drafts::images`。
- `catmail-proto`：`Outgoing::html`、`OutgoingAttachment::content_id`，`body()` 拼 alternative / related / mixed，HTML
  套文档头并限制图片宽度。
- `catmail-core`：`DraftEditor::attach_image`（只收 `named_as_image` 的文件名，返回附件和它的文件）、`images`；构建时带上 HTML 和 Content-ID。
- `catmail-frontend`：`composer::rich_text`（`text_to_html`、`doc_to_text`、`Images`）；`Opening::images`、`Fields::html`。
- `catmail-gui`：`composer/rich.rs` 包装编辑器会话；正文换成 `rich_editor()`；底栏加插入图片和五个格式按钮（Heroicons 的
  bold、italic、underline、list-bullet、numbered-list）；粘贴的图片进正文、其他文件当附件；附件栏不列内嵌图片，草稿右栏
  用图片图标标出。
- `catmail-mcp`：草稿输出加 `formatted`，附件加 `in_text`；`update_draft` 的 `text` 说明会去掉格式。
- 测试：MIME 结构（`html_goes_with_its_text_and_the_images_it_shows`）、引擎端到端
  （`html_is_sent_with_the_images_it_shows`）、`DraftChanges` 的语义、`rich_text` 的转换、`Rich` 的格式命令与图片往返。
  GUI 窗口没有无头测试（ROADMAP 里的 nsh-testing 一项），格式按钮、粘贴、图片显示需要手动在应用里试。
