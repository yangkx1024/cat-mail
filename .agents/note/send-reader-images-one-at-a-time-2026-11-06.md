# 读信页面的图片一次只发一张，页面按 URL 一次找出图片

- 日期：2026-11-06
- 类型：修复

## 背景

一封邮件的远程图片显示过（已在缓存里）以后，切到别的邮件再切回来，图片还是一张张慢慢出来，像从网络取一样；
这期间切到别的邮件会卡，这封邮件还会在 web view 上停留很久。

本机图片缓存里 34 张图共 43.6 MB，单张中位数 1.3 MB、最大 4.5 MB。nsh 没有自定义 URL scheme，图片只能以
`data:` URL 写进 `evaluateJavaScript` 的一段脚本（macOS、Linux 都是：`js_string` 逐字符转义成字符串字面量）。
每个字节在 GUI 主线程上 base64、转义、转 NSString / 发 IPC，在网页进程里被 JSC 解析、`img.src` 再解析一遍、
再解码。页面载入时 `ready()` 把所有图片一次发出去：主线程一口气处理几十 MB（界面卡），网页进程里排满这些脚本，
新页面的载入排在后面（旧邮件停留不走），发出去的消息撤不回来。

另外页面脚本每收到一张图就对所有 `<img>` 做一次 `new URL(src)`，已填上的 `src` 是几百 KB 的 `data:` URL，
n 张图是 O(n²)。这个开销是真的，但只是次要原因：只改它（第一版）之后仍然很慢、仍然会卡。

## 决策

- 一次只发一张：页面处理完一条 `i:` 就回 `d:<页号>`（放在 `finally` 里，出错也回，否则后面的图都不来），
  `PageImages` 收到再发下一张。换邮件时 `forget_message` 清掉队列，剩下的不再发；重建页面时队列重置，新页面
  `r` 之后重新排。这样主线程的工作分散到多次事件循环里，网页进程里最多排一张。总的载入时间不变（还是和字节数
  成正比），但从上到下按文档顺序出来，换邮件不再等。
- 远程图片存原始字节，发送时才编成 `data:` URL：邮件打开期间一直存着，base64 多三分之一，编码也跟着分散开。
- 页面第一次收到图片时把 `<img>` 按规范化后的 URL 建一张表（`Map`），之后只查表。`data:` 的 `src`（建页时嵌进去的
  内嵌图片）不进表：不会有消息按 `data:` URL 发来。页面里的图片只有这个脚本会改，表不会过时。
- 真正变快要让 WebKit 自己读图片（URL scheme handler：macOS `WKURLSchemeHandler`，Linux
  `webkit_web_context_register_uri_scheme`），nsh 还没有，已提 longbridge/nsh#1136；修好后要改的地方记在
  `crates/catmail-gui/ROADMAP.md` 和 `gui.md` 的「已知问题与上游」。没有选择缩小图片：会改变清晰度。

## 方案

- `crates/catmail-frontend/src/html/images.rs`：`PageImages` 加队列和 `sending`，`ready` / `add_inline` /
  `add_remote` / 新的 `taken` 返回 `Option<String>`；远程图片存 `RemoteImage`。
- `crates/catmail-frontend/src/html/page.rs`：`BRIDGE` 的 `imagesOf(url)` 和 `d:`，`FromPage::Taken`。
- `crates/catmail-gui/src/reader/`：`d:` 接到 `image_taken`。
- 文档：`docs/architecture/gui.md`、`crates/catmail-gui/ROADMAP.md`。
- 验证：`PageImages` 的测试改成一张一张收（`one_image_at_a_time`、`another_message_stops_what_is_left` 等），
  `the_page_finds_its_images_once`、`the_page_says_it_has_taken_each_image`、`FromPage` 解析 `d:`。用 Node 加假 DOM
  跑新旧脚本：62 张图每张 200 KB，填上的图片一样，处理时间从约 155 ms 到约 2 ms，且每张都回了 `d:`。
