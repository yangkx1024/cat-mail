# 读信区的 cid: 图片 URL 改在 Rust 里算出，发给页面的图片移到 frontend

- 日期：2026-11-06
- 类型：重构

## 背景

issue #246。读信区先显示文字、内嵌图片后到：页面载入后注入脚本把每个 `cid:` 图片报一遍（`c:<页号>:<url>`，
必须在 `r:` 之前），Rust 记下 URL 的写法，图片取到后按这个 URL 发回（`i:`）。这次往返只是为了让 Rust 知道 URL
在页面里怎么写，为此多了一种页面消息、`page_refers_to`、和"`c:` 先于 `r:`"的时序约定。

去掉往返之后，读信区里"哪些图片什么时候发给页面"的状态（取到的内嵌部分、显示了的远程图片、页面缺哪些、页面
载入了没有）就不再依赖 nsh，按约定应该放进 `catmail-frontend`，而它之前没有任何测试。

## 决策

- 在 Rust 里算：`catmail_html::embed_inline_images` 嵌入时顺便记下没嵌的 `cid:` 图片（`InlineUrl`），像
  `remote_image_url` 一样用 `url::Url` 规范化。不另走一遍清洗器：建页本来就在 GUI 线程上清洗好几遍，大邮件多
  一遍很明显。`url` 和 WebKit 都按 WHATWG URL 解析，`new URL(src).href` 和它写法一致；测试里的期望值（scheme
  大小写、百分号编码、非 ASCII、空格、`?`/`#`、首尾空白）用 Node 的 `URL` 逐个对过。
- 放弃"`i:` 带 Content-ID、脚本自己匹配"：Content-ID 的解析规则（RFC 2392、百分号解码）要在 JS 里再写一份。
- 只记没嵌进页面的：已嵌入的图片在页面里是 `data:`，原来脚本也不会报它们；算进去的话页面载入时会把已有的
  图片再发一遍（可能几 MB）。嵌入、`cid_image_ids`（算哪些部分是内嵌的）和这份列表都用同一个 `cid_image_id`
  取 Content-ID（先去掉首尾空白），原来嵌入时不去空白，`src=" cid:x "` 建页时嵌不进去。
- 发给页面的图片由 `catmail_frontend::html::PageImages` 管：它建页面，有图片来就返回要发的消息，页面没载入完
  就留到 `r`。GUI 只剩 web view、`post_message`、取图片的任务和"还是不是这封邮件"的判断。换邮件时只清内嵌和
  远程图片（和原来一样），页面的 URL 和载入状态在建下一个页面时才重置。
- `body_page`、`image_message` 只在 `html` 模块里用，不再导出；`content_id_from_url` 也不再从
  `catmail_core::html` 导出，前端不用自己解析 `cid:`。

## 方案

- `crates/catmail-html/src/images.rs`：`embed_inline_images` 返回 `Embedded { html, inline_urls }`，
  `InlineUrl { content_id, url }`，每个 URL 一次，按文档顺序。
- `crates/catmail-frontend/src/html/images.rs`：新的 `PageImages`（`page`、`ready`、`add_inline`、
  `add_remote`、`forget_message`）。
- `crates/catmail-frontend/src/html/page.rs`：`BRIDGE` 不再报 `c:`，去掉 `FromPage::Inline`。
- `crates/catmail-gui/src/reader/`：`Page` 里的图片状态换成 `PageImages`，删掉 `page_refers_to`。
- 文档：`docs/architecture/gui.md`、`docs/architecture/testing.md`、`crates/catmail-gui/ROADMAP.md`。
- 验证：`leaves_inline_images_not_given_as_a_page_spells_them`（catmail-html）；`PageImages` 的四个测试
  （载入前来的等 `r`、载入后来的立即发、换外观重建时嵌入已取到的而不重发、换邮件清空）；`c:` 现在被当作未知消息。
