# 读信区的 web view 不留网站数据

- 日期：2026-11-07
- 类型：功能

## 背景

正文是别人写的 HTML。读信区的 web view 原来和应用共用 nsh 按 app id 建的持久 `NetworkSession`
（Linux：`$XDG_DATA_HOME/<app id>/webkit`、`$XDG_CACHE_HOME/<app id>/webkit`），页面留下的网站数据会写进去。
nsh main 加了 `WebView::incognito`（longbridge/nsh#1028）。

## 决策

- 打开 `incognito`：Linux 上是这个 web view 自己的临时 `NetworkSession`，macOS 上是不持久的
  `WKWebsiteDataStore`，数据只在内存里，随 web view 没了。页面 JS 已关、CSP 只放行 `data:`，实际留下的东西本来
  就很少，这一步是不让它和应用共用一份持久数据。
- 临时 session 属于 web view，随它结束，不影响 #1051 带来的关窗后网络进程退出。读信区只有一个 web view，随主窗口
  存在，换邮件不重建，所以不多花启动开销。

## 方案

- `crates/catmail-gui/src/reader/view.rs` 的 `web_view` 加 `.incognito(true)`（只在建 web view 时读）。
- gui.md「正文在 web view 里」加"网站数据"一条。
- 验证：macOS 上 clippy、测试通过；还要在两个平台上开一封信确认正文照常显示，Linux 上确认不再建
  `~/.local/share/net.yangkx.catmail/webkit`。
