# 读信区用 Blitz 排版 HTML

- 日期：2026-10-22
- 类型：功能

## 背景

gpui-kit 的 `TextView` 只排 HTML 的一个子集，营销邮件的行内样式、背景、内边距基本丢失。需要一个
按浏览器方式排版、同时兼容 Wayland 和 Windows 的方案。

## 决策

- 不嵌 WebView：wry / gpui-webview 在 Linux 上只支持 X11（整个应用要跑在 XWayland 上），Windows 上
  盖住弹出层，还要引入 gtk3-rs（RustSec 标为 unmaintained，glib 0.18 unsound）。
- 进程内引擎画成像素。先并排接入 litehtml 与 Blitz 对比：litehtml 绑定的 CPU 容器（cosmic-text 0.14）
  在开发机上把汉字回退到等宽字体、中文字距偏大，还需要 C++ 编译；Blitz 的表格、中文和字体选择都正常。
  选 Blitz，litehtml 删除；对比经过记在 `docs/architecture/gui.md` 的"选型记录"。
- 新建 `catmail-render` crate，不依赖 GPUI 和邮件类型：Stylo 编译重，单独编译、单独测试；`Page`
  trait 加一个打开函数是引擎接口。
- 画成像素而不是翻译成 GPUI 图元：实现量小，代价是暂时不能选择文字，所以保留 GPUI Kit 作为可切换的
  备选，HTML 默认用 Blitz。
- 每个文档一个线程（文档不是 `Send`），`Document` 句柄丢掉即退出；调用方放弃的请求直接跳过。页面按可见区域分块绘制，长邮件
  不整页占内存、不超出纹理尺寸。
- 引擎不联网：图片只经读信区的 `Resolver`，与 `TextView` 共用 `html::Carried` 规则；关掉 blitz-dom
  的 `svg` feature（usvg 按本地路径读嵌套图片）。

## 方案

- `crates/catmail-render`：`worker.rs`（`Document`、请求、线程循环）、`blitz.rs`
  （`NetProvider` 只问 resolver，分块靠视口滚动、链接靠命中测试）、`pixels.rs`（预乘 RGBA 叠白底转 BGRA）。
- `catmail-gui/src/reader/page.rs`：`PageView`，绘制时测量宽度和可见区域，按需排版、取分块，悬停和
  点击问引擎链接；淘汰、重排、释放时 `drop_image`，避免 sprite atlas 泄漏。
- `reader/mod.rs`、`view.rs`：`HtmlEngine`（Blitz / GPUI Kit）选择，跨邮件保留；`show_page` 让页面跟随
  邮件、引擎和后到的内嵌图片。
- 验证：`catmail-render` 单元测试（分块与缩放、窄页更高、文字、链接命中、图片只经 resolver、慢文档
  不阻塞别的文档、panic 只停一个文档）；GUI 集成测试 `switches_the_html_between_engines`；截图测试默认即 Blitz，另拍一张
  GPUI Kit。`cargo deny` 通过。
