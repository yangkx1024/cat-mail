# 按需逐张加载并缓存远程图片，读信页面换页不闪

- 日期：2026-10-22
- 类型：功能

## 背景

读信区一律拦截远程图片，格式栏只有一行"Remote images are not loaded"，营销邮件大多靠远程图片排版，
没法看全。需要一个按钮，由用户对单封邮件决定加载。

同时处理 issue #70：`catmail-render` 的 `Resolver` 返回 `Vec<u8>`，`Images::bytes` 每次打开文档都把
每张图 `to_vec` 一份，而文档会因图片到达、主题、缩放反复重开。远程图片也走这条路，一并改。

图片要逐张出现（下载完哪张显示哪张），而且不能闪：原来每次图片到达都重开文档，新排版一回来就先
`drop_strips`，新分块画好前页面是空白的（主题切换、内嵌图片到达时同样会闪）。

加载过的图片要缓存在本机，再打开直接显示、不再请求；设置里能清除。

## 决策

- 下载放在引擎：`MailClient::fetch_remote_images`，在引擎运行时上跑（GPUI 不是 tokio），HTTP 客户端
  在 `catmail-proto` 的 `http` 模块（与 IMAP / SMTP 同属网络协议层）。
- 用 reqwest 0.12、关掉默认 feature，`use_preconfigured_tls` 交给它邮件服务器用的同一个
  rustls + ring 配置；不引入 native-tls、aws-lc，`cargo deny` 通过（hyper 等已经经 gpui-kit 在依赖树里）。
  没选 0.13：默认 aws-lc-rs。没有直接用 hyper：重定向、超时都要自己写。
- 隐私：只 GET，不存 cookie，关掉 `Referer`（reqwest 默认在重定向时发），User-Agent 不冒充浏览器。
- 限制：每张 30 秒、10 MiB，同时 6 个，一次最多 200 张。类型优先按开头字节认，服务器常标错。
- 逐张显示：`fetch_remote_images` 返回流（tokio channel，任何执行器都能等），按完成顺序给结果；
  GUI 每到一张就连同已到的（`next_ready`）合成一批。丢掉流（换邮件时任务丢弃）即中止剩余下载。
- 不重开文档：Blitz 的 `NetHandler` 可以晚些再调用，于是 `NetProvider` 把答不上的请求按 URL 留着，
  `Document::set_images` 换 resolver 后重问一遍，Blitz 下次排版只做增量。放弃了"每批重开文档"：
  每次都要整页解析、排版，图片多时很慢。
- 不闪：新排版只画屏幕上的分块，到齐后与排版一起整体替换；上一个排版没显示前不开始下一次排版，
  连续到达的图片合并进下一次，不会让页面一直换不过去。这同时修好了切主题、内嵌图片到达时的空白帧。
  新 resolver 先留在 `PageView`，重新排版时才交给文档：一到就交，线程会作废缓存的排版，正在画的
  新排版的分块就按更新的图片重排，与它的高度对不上。屏幕上没有分块要画的新排版（页面空了）在测量
  的那一帧画完后直接换上，否则永远等不到分块、旧页面一直留着。
- 点击只对这封、这次打开有效，不记住发件人；"在浏览器打开"仍按 CSP 拦截。
- 缓存：数据目录的 `remote-images/`，一个 URL 一个文件（SHA-256 命名，内容是 MIME 类型 + 换行 + 字节）。
  放弃了 SQLite 表 + `BlobStore`：blob 按内容寻址，可能与同内容的附件共用，只能等 GC 宽限期（1 小时）
  后删，"清除缓存"做不到立刻释放；独立目录没有别的引用，随时可删，缺文件就当没命中。
- 缓存命中直接显示、不用再点：不发请求就不给发件人任何信号。按 URL 而不按邮件缓存，也不看
  `Cache-Control`（照 `no-store` 再取一次反而又暴露一次）。上限 256 MiB，按最后使用时间淘汰。
- `ImageCache::new` 不做 IO、第一次写入时才建目录，免得 `Engine::new` 再加一个参数。
- 不拦截局域网 / 本机地址：只在用户点击后对这封信的图片 GET，与浏览器打开网页相同。
- URL 按 `url` 解析后的写法存取：Blitz 先解析再问（主机名小写、空格转义），`TextView` 原样问。
- issue #70：`Resolver` 改返回 `bytes::Bytes`（`catmail-render` 重新导出），`Images::bytes` 用
  `Bytes::from_owner` 包住 `Arc<Image>`，不再额外存一份。

## 方案

- `catmail-proto/src/http.rs`：`ImageFetcher`、`RemoteImage`、`Error`（SVG 算 `NotAnImage`）；`tls.rs` 加
  `client_config`；`mime/html.rs` 加 `remote_image_urls` / `remote_image_url` / `MAX_REMOTE_IMAGES`：
  哪些 URL 是远程图片、怎样规范化，由 proto 一处决定，core 的缓存和 GUI 都用它。
- `catmail-testkit/src/http.rs`：`FakeHttp`（按路径回答或挂起请求，记录请求头、放行、统计被放弃的
  挂起请求），proto / core / GUI 的测试共用。
- `catmail-core`：`images.rs`（`RemoteImageStream`；先给缓存命中的，其余 JoinSet + Semaphore 并发、
  成功的写入缓存，`select!` 等接收端关闭以便立即中止，结束后按上限淘汰）、`image_cache.rs`
  （`ImageCache`：get / get_many / put / size / clear / trim，按规范化的 URL 存）、`Config::image_cache_dir`、
  `Engine::image_fetcher`（首次使用时创建）、`Error::Http`；`MailClient` 加
  `cached_remote_images`、`image_cache_size`、`clear_image_cache`。
- `catmail-render`：`Resolver = Fn(&str) -> Option<Bytes>`；`blitz.rs` 的 `Images` 留住等待中的
  handler，`Page::set_images` / `Document::set_images`（worker 的 `Job::Images`，答上了就作废缓存的排版）。
- `catmail-gui/src/reader/page.rs`：`set_images` 取代 `set_source`；`Next` 与 `show_next` 双缓冲换页，
  `Opened::changed` 控制重排时机。
- GUI：`remote_image_urls` 取代 `has_blocked_images`；`OpenMessage` 记录还没显示的远程图片 URL 和
  `loading_remote`；`Reader::show_cached_remote_images`（与内嵌图片并行）、`show_remote_images`；
  `Images::with_inline` / `with_remote`，先后到达时互相保留，沿用已解析的 URL；`add_remote_images`
  逐批合并；格式栏的提示换成 "Show remote images" 按钮（加载时转圈，没有要显示的时消失）。Settings › Reading（`settings/dialog/reading.rs`）显示缓存大小和
  "Clear image cache"。
- 文档：`docs/architecture/bodies.md`、`gui.md`、`architecture.md`、README。
- 验证：proto 单元测试（重定向后不带 cookie / Referer、按字节认类型、拒绝非图片 / 404 / 非 web
  scheme、到 `file:` 的重定向不跟、超大与超时）；core 集成测试
  `fetches_remote_images_as_they_come_until_dropped`（按完成顺序给出、丢掉流后挂起的请求被断开）、
  `remote_images_shown_once_come_from_the_cache_until_cleared`（命中不发请求、失败的不缓存、清除后重新取）；
  `image_cache` 单元测试（按 URL 存取、坏文件丢弃、按最后使用时间淘汰）；
  render 测试 `images_given_later_show_in_the_open_document`（不重开，页面变高、像素变色、已答的不再问）；
  GUI 单元测试（远程图片两种 URL 写法都能取到、SVG 不显示、`Bytes` 与 `Image` 共用内存）、
  `images_that_come_replace_the_page_without_a_blank`（每一帧都有分块且高度与所显示的排版一致；恢复旧的
  "排版一回来就丢分块"时它失败）、`images_that_come_while_a_layout_is_made_go_to_the_next`（排版途中
  又到的图片不混进这次排版的分块）、`a_page_with_nothing_to_paint_replaces_the_last`（缩小文字使媒体
  查询隐藏整页后，空页面换上；两者去掉修复都失败）和窗口测试 `shows_remote_images_as_they_come_when_asked`（点击前不发请求；
  快的一张先显示、慢的放行后再显示，按钮在全部结束后消失；换一封再回来，图片从缓存直接显示、不再
  请求）、`clears_the_image_cache`（设置里清除后缓存为空）。
