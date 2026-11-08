# Linux 上读信页面的图片限流

- 日期：2026-11-08
- 类型：修复

## 背景

图片改由 `catmail-image:` scheme 应答后（同日的 serve-reader-images-through-url-scheme），图片多的邮件在 Linux 上
仍然慢：打开约 1 秒才看到第一张图，图片出来之前切到别的邮件会卡。复现：一封 25 张远程图片（都在缓存里，
共 48 MB，多是 2000 万像素以上的照片）的邮件，打开后马上切走。

计时日志（release 构建）显示我们这边 45 ms 内就应答完了全部 25 张，但切到下一封时，新页面要过 0.1–3 秒才开始
请求图片。用最小的 WebKitGTK 程序复现（和 nsh 一样 `MemoryInputStream` + `finish_with_response`，web view 和
页面一样高）：

- 原因：WebKitGTK（`WebKitURISchemeRequest.cpp`）把应答按 8 KB 一块读、每块一条 IPC 发给网页进程，显示着的
  大图每来一块就重画一次；48 MB 约 5900 块，新页面排在后面。
- 同样的图经 HTTP 加载，切换只要 3–135 ms；scheme 一次全应答 463–1021 ms。
- `decoding="async"`、只用 `fetch` 取字节对比过：不是解码本身，也不是单纯传输，是传输加逐块重画。

## 决策

- 限流：同时在传的不超过 1 MiB（一张更大的单独传），按页面请求的顺序，页面报一张到了（或失败了）再放行。
  复现程序里切换 12–254 ms；catmail 里实测第一张图从约 1 秒到 35–93 ms，切换最坏从 2973 ms 到约 365 ms。
- 只在 Linux 上限：macOS 上 nsh 把整张图作为一个 `NSData` 交给 `WKURLSchemeHandler`，一张图一条 IPC，没有
  这个问题；限流只会让 WebKit 不能并行解码。上限由 GUI 按平台给（`IMAGES_IN_FLIGHT`），`BodyPages::new`
  接受 `None`（不限）。
- 页面回报用单独的注入脚本 `WATCH_IMAGES`，在页面内容之前注入（nsh 的 `injected_java_script` 在文档结束时才
  运行，之前就到了的图片会漏报），只在限流时注入。只报读信区自己的图片（`l:<URL>`，URL 里已经带页号）：
  `data:` 图片的 `src` 有几 MB，报回来只会被丢掉。
- 有图片被压着时，一个任务每 3 秒看一次有没有进展，没有就放行剩下的并记 warn：某张图不报，后面的就永远
  不来。只在发出图片后才挂计时器的话，会漏掉"一张到了但仍超限、另一张一直不报"的情况。
- 限流单独放在 `html::pacer::Pacer`，`BodyPages` 只是用它：它是过渡办法，WebKitGTK 按大块读之后整块删掉，
  不动 `BodyPages`。已给 WebKit 报 bug（https://bugs.webkit.org/show_bug.cgi?id=330624）。
- 不放进 nsh 的 Linux 后端（同日在 nsh 里试过再撤掉）：WebKit 的 `resource-load-started` 给的
  `WebKitWebResource` 对 scheme 的子资源也报 `finished` / `failed`（和页面的 `load` 只差几毫秒），nsh 可以按它
  放行（监视流读完不行：UI 进程不等网页进程，24 个流 60 ms 内就读完了）。量下来光限流不够：放行持续到新页面
  提交的话，切走后放出的一张大图会先被完整解码，早切换时反而比一次全应答慢（约 0.5 秒对约 0.1 秒）。必须在导航
  开始时就停发，加上这一条后切换中位数 582 → 245 ms，和这里相当。但哪次导航是换页面只有应用知道：读信区建新
  页面时就清空队列、不理旧页面的回报；nsh 只能猜（用户手势当主框架导航会误伤 iframe 里的点击，猜错等 3 秒
  兜底，触控板滑动前进后退抓不到），还会把所有用 scheme 的应用的加载都串行化（整页加载从约 0.25 秒变成
  1.5–2 秒），并依赖 WebKit 没写进文档的时序。没有第二个应用受影响，所以留在应用层；以后要放进 nsh，应做成
  可选，并由应用告诉它何时换页面。
- 放弃的方案：图片传完前藏起来（量过，加在限流上没有帮助）；缩小图片（改变清晰度）；web view 改成自己滚动、
  只画看得见的图（读信区的大改）；本地 HTTP 服务器（安全面大）。根上的修法在 WebKitGTK（按大块读）。

## 方案

- `crates/catmail-frontend/src/html/pacer.rs`：`Pacer`（`queue`、`send`、`loaded`、`holds_back`、`unstick`、
  `clear`）。
- `crates/catmail-frontend/src/html/pages.rs`：`BodyPages::new(in_flight)` 用 `Pacer`，`loaded`、`holds_back`、
  `progress`、`unstick`。
- `crates/catmail-frontend/src/html/page.rs`：`WATCH_IMAGES`，`FromPage::Loaded`（`l:<URL>`）。
- `crates/catmail-gui/src/reader/`：`IMAGES_IN_FLIGHT`（Linux 1 MiB，其他 `None`），限流时注入 `WATCH_IMAGES`，
  `image_loaded`，`watch_images`（`UNSTICK_AFTER`）。
- 文档：`docs/architecture/gui.md`（图片、脚本、已知问题）、`crates/catmail-gui/ROADMAP.md`。
- 验证：`Pacer` 的测试（大图一张张、按顺序；小图一起；重复的回报不算；不限时一到就发；没有进展才放行；换页面
  交回没发的）、`BodyPages` 的（按请求的顺序、别的页面和不是读信区的 URL 不算、换页面交回没发的）、`l:` 的
  解析、`WATCH_IMAGES` 监听 `load` 和 `error`、只报读信区的图片。Linux 上用真实邮件和临时计时
  日志量过（数字见上）。macOS 还没在图片多的邮件上看过。
