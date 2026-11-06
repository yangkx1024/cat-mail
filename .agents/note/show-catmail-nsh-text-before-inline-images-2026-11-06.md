# nsh 版读信区先显示文字，内嵌图片后到

- 日期：2026-11-06
- 类型：功能

## 背景

nsh 版要等这封邮件的内嵌图片（`cid:`）全部取完才建页面，图片多或大时文字也要跟着等。gpui 版是先显示文字，内嵌
图片取到一张显示一张。nsh 的页面一旦建好，换内容就得重新加载，所以不能像 gpui 版那样重建页面。

## 决策

- 和远程图片走同一条路：注入脚本收到 `i:<url>\n<data:>` 就把 `data:` URL 填进对应的 `<img>`，页面不重新加载。
- Rust 要知道页面里 `cid:` 图片的 URL 怎么写，才能按 URL 发回去，所以页面加载完先报一遍：每个不重复的 `cid:`
  图片发一条 `c:<页号>:<url>`，在 `r:` 之前。Rust 用 `catmail_html::content_id_from_url` 换成 Content-ID。
  想过在 Rust 里直接从 HTML 算出这些 URL，省掉这次往返，但要保证写法和 `img.src` 逐字一致，另开了 issue 再做。
- 已经取到的内嵌图片在页面重建（换外观）时直接嵌进页面，不用等页面报完再发。
- 每张图只存一份字节，`data:` URL 在发送时才生成，不在邮件打开期间一直多存一份 base64。

## 方案

`crates/catmail-nsh/src/page.rs`（BRIDGE 报 `c:`，`FromPage::Inline` 和测试）、`crates/catmail-nsh/src/reader.rs`
（`Page::inline`、`Page::cids`，`open_message` 先建页面再逐张取内嵌图片，`add_inline_image`、`page_refers_to`、
`page_ready`）。验证：打开带内嵌图片的邮件，文字先出现，图片随后补上；切换外观后图片仍在。catmail-nsh 的 clippy
`-D warnings` 和测试通过。
