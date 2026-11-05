# 试验：基于 nsh 的前端，正文用 WebView

- 日期：2026-11-05
- 类型：功能

## 背景

想看看不用 GPUI、改用 [Native Shell（nsh）](https://nsh.dev/) 在 Linux 上效果如何。nsh 是 longbridge 的 Rust UI 框架，Linux 后端是 GTK 4 原生控件 + Pango，布局用 Taffy，状态是 SolidJS 式的 signal。这是 `nsh-spike` 分支上的试验，不合入 `main`。

## 决策

- **新建独立的 `crates/catmail-nsh`**，不动 `catmail-gui`。只做只读三栏：文件夹、虚拟列表、正文，用来对比观感、内存和滚动表现。
- **正文先用 nsh 的 WebView（WebKitGTK 6.0）**，没有走 Blitz 的 tile。nsh 没有能直接显示像素缓冲的元素：Blitz 要么编成 PNG 再塞进 `data:` URI，要么每个平台写一个原生组件。WebView 出效果最快，排版也更完整。
  - 代价是 WebKit 有自己的网络，而 nsh 只检查顶层导航，不管子资源。所以由页面自己断网：`<head>` 最前面放一条 CSP（`default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:; form-action 'none'; base-uri 'none'`），要显示的图片先嵌成 `data:` URL（目前只嵌了内嵌部分）。保证从"引擎根本没有网络"降成了"靠 CSP 拦"。
  - 页面自身的 JS 关掉（`enable-javascript-markup`）。注入的脚本 WebKit 照样执行，用它通过 `window.nsh.postMessage` 回报页面高度（`h:`）和被点击的链接（`o:`，交给系统浏览器打开）。
  - WebView 设为不滚动，高度跟着页面走，信头和正文由外层 scroll 一起滚。
  - 暗色沿用 `recolor_html`；颜色来自 CSS 变量、没法重新着色的邮件保持亮色。
- **不放进主 workspace**：和 Blitz 一起编译时会打开 taffy 的 `calc` feature，而 nsh-core 0.4.0 的 `core/sticky.rs` 里有个 match 漏了 `Calc` 分支，编不过。所以它自成一个 workspace，复制了 root 的 imap-proto patch，通过 `.cargo/config.toml` 共用 root 的 `target/`。
- 放弃的方案：nsh 的 GPUI 后端（试验目的就是不用 GPUI）；TSX 写法（要带 QuickJS，我们全是 Rust）。

## 方案

- `crates/catmail-nsh/src/main.rs`：打开 `MailClient`（后台同步），用 `run_app` 起 GTK 窗口。数据目录默认和 GUI 相同，可以用 `CATMAIL_DATA_DIR` 换。
- `src/app.rs`：状态放在 nsh global 里。`MailClient` 的 future 直接在 nsh 的 `spawn` 里 await（UI 线程上的本地 executor，不是 tokio），引擎照常在自己的 runtime 上干活。选中文件夹或邮件通过 `effect` 起任务，换选中项时，还在加载的旧任务随 effect 重新运行被取消。收到引擎事件后，250ms 内的事件合并成一次刷新。
- `src/page.rs`：拼出正文页（CSP、我们自己的基础样式、内嵌图片），以及和注入脚本之间的消息协议。
- 仅供试验调试的环境变量：`CATMAIL_NSH_OPEN=<序号>` 自动打开列表里第几封；`CATMAIL_NSH_HTML=<文件>` 用这个文件的 HTML 替换正文；`CATMAIL_NSH_APPEARANCE=dark|light` 强制外观。
- `flake.nix` 的开发 shell 加了 `gtk4`、`webkitgtk_6_0`。

验证：

- `page.rs` 有单元测试：CSP 在正文之前、`cid:` 图片被嵌入、纯文本被转义、消息协议解析。
- 在本机 niri（Wayland）上用 4 个真实账户（QQ、Gmail、163、iCloud）跑过，三栏、中日文、HTML 邮件排版、暗色重新着色都正常。
- 用探针页（`<img>`、`srcset`、`<picture>`、`@import`、`@font-face`、CSS 背景、`<link>`、iframe、object、表格 `background`、`meta refresh`、script……）指向本机 HTTP 服务器测试：开着 CSP 时 0 个请求；把 CSP 换成放行全部后，WebKit 立即请求了 `/img.gif`，说明测试方法本身有效。
- 内存（debug 构建，打开一封 HTML 邮件）：主进程 PSS 约 176MB，WebKitWebProcess 约 110MB，NetworkProcess 约 33MB。

已知问题：

- 在 nixpkgs 的 GTK 4.22.4 上，nsh 会刷大量 `Allocating size … without calling gtk_widget_measure()` 警告。
- 用 root 的 `deny.toml` 跑 cargo deny：bans 和 sources 通过；licenses 不通过（webpki-roots 的 CDLA-Permissive-2.0，target-lexicon 和 winx 的 Apache-2.0 WITH LLVM-exception）；advisories 有 bincode 1.x 已停止维护（经 nsh-base → syntect）。
- 还没做：远程图片（下载后嵌成 `data:`）、操作、写信、托盘、键盘导航、macOS / Windows。
