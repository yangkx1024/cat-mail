# 读信页面的图片改由自定义 URL scheme 应答

- 日期：2026-11-08
- 类型：重构

## 背景

nsh 的 web view 原来没有自定义 URL scheme，读信区只能把图片编成 `data:` URL 写进 `evaluateJavaScript` 的脚本
发给页面（`i:` 消息），每个字节都要在 GUI 主线程上 base64、转义，再被 JSC 解析一遍；为了换邮件不卡只好一次发
一张（`d:` 回执），图片多的 newsletter 载入时间和字节数成正比。为此提了 longbridge/nsh#1136，上游 #1233
（`WebView::url_scheme`、`nsh_core::web_resource`）已合入 main。

## 决策

- nsh 升到 main 最新的 `71d0b11b`（和以前一样跟 main，不只到 #1233 的合并点）。顺带进来的 longbridge/nsh#1164
  把 nsh-updater 换成 zip 9，`deny.toml` 里 zip 8 的四条 advisory 忽略一起删掉。
- 页面里的 `cid:` 图片和前 `MAX_REMOTE_IMAGES` 张远程图片在建页时改写成
  `catmail-image:<页号>/cid/<Content-ID>`、`catmail-image:<页号>/remote/<URL>`。键里除 unreserved 字符外都百分号
  编码，URL 解析器不会再改写它，页面请求的写法就是建页时的写法；用不透明路径（没有 `//`），`..` 这样的
  Content-ID 不会被当成点段去掉。带页号：上一个页面晚到的请求能认出来丢掉。
- 没到的图片，请求先留着，图片到了再应答，包括用户还没点"Show remote images"的远程图片。ROADMAP 原来的
  计划是先让请求失败、点了之后让页面重新请求；放弃它，因为那要脚本改 `src`，还要绕开 WebKit 对失败资源的内存
  缓存（同一 URL 再设一次可能不重新请求），等于把 `i:` 消息换个样子留下来。留着请求只让页面的 `load` 事件晚到，
  报高度靠 `ResizeObserver`，不受影响。
- 换邮件不撤留着的请求：旧页面在新页面画好之前还显示，应答失败会闪出破图。它们随旧页面被 WebKit 取消，下一个
  页面的请求来时（或再建一页时）丢掉。
- handler 在平台主线程上、视图之外调用，且要 `Send + Sync`，碰不到读信区的 `Rc` 状态，所以只把请求送进
  `futures_channel::mpsc`，由 web view 所在组件里 `spawn` 的任务应答（随组件结束）。
- `BodyPages`（原 `PageImages`）对请求的类型泛型，`catmail-frontend` 里不出现 nsh 的类型，测试用 `&str` 当
  请求。页号也只在它那里：建页、建空白页时分配，`is_shown` 判断页面消息是不是当前页面的，原来 GUI 和它各数
  一份。留着的请求分两份：当前页面的（`waiting`）和被替换页面的（`replaced`，新页面第一次请求时丢掉）。
- `serve_images` 改写页面时一并返回远程 URL（`Served::remote_urls`），删掉 `remote_image_urls`：读信区要下载的
  和页面里改写的是同一批，规则（去重、最多 200 张）只写一处，建页少过一遍清洗。
- URL 里键的百分号编解码用 `percent-encoding`（`url` 本来就依赖它，版本相同），不手写：编码这次才需要，
  原来手写的解码（`cid:` URL 用）一起换掉。
- `InlineImage` 改名 `Image`，存类型和 `Arc<[u8]>`，不再预先编成 `data:` URL：应答不复制字节，只有"在浏览器
  打开"才编码。远程图片进来时也转成它（类型先按内容嗅探）。

## 方案

- `crates/catmail-html/src/images.rs`：`Image`、`ServedImage`（`path` / `from_path`）、`serve_images` 和
  `Served`；`embed_inline_images` 只返回 HTML，去掉 `Embedded`、`InlineUrl`、公开的 `data_url`、
  `remote_image_urls`。`catmail-core` 的
  `html` 模块跟着改导出。
- `crates/catmail-frontend/src/html/pages.rs`（原 `images.rs`）：`IMAGE_SCHEME`，`BodyPages<R>` 的 `page`、
  `blank`、`is_shown`、`request`，`add_inline`、`add_remote` 返回要应答的请求；去掉队列、`ready`、`taken`。
- `crates/catmail-frontend/src/html/page.rs`：CSP 的 `img-src` 放行 `catmail-image:`；`BRIDGE` 去掉 `r:`、`d:`
  和收图片的部分，`FromPage` 只剩 `Height`、`Painted`、`Open`。
- `crates/catmail-gui/src/reader/`：web view 加 `url_scheme`，`serve_image` / `answer` 应答；去掉 `post`、
  `page_ready`、`image_taken`、web view 的 `ElementRef` 和 GUI 自己的页号。
- 文档：`docs/architecture/gui.md`、`docs/architecture.md`、`background.md`、`testing.md`、
  `crates/catmail-gui/ROADMAP.md`、根 `Cargo.toml` 的注释。
- 验证：catmail-html 的 `serves_inline_and_remote_images_from_the_base`（逐个用 `url` 的 WHATWG 解析器确认 URL
  不被改写、能解析回来）、`serves_as_many_remote_images_as_a_reader_shows`、
  `a_served_path_names_only_what_could_be_served`；frontend 的 `BodyPages` 测试（到了再应答、重建页面立即应答、
  只应答当前页面、换邮件清空、不认识的 URL 丢掉）。`cargo test --workspace`、clippy `-D warnings`、
  `cargo deny check` 通过。还没在真实窗口里跑过，见 ROADMAP。
