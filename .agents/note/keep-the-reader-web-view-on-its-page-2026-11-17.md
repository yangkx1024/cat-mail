# 读信区的 web view 不再被右键菜单、拖放带到远程页面

- 日期：2026-11-17
- 类型：修复

## 背景

#322。拦链接的只有 `BRIDGE` 的 `click`。nsh 给 Rust 应用的策略是 `Policy::default()`（`Capabilities::all`），
web view 的顶层导航全部放行，应用改不了。右键邮件里的链接选 WebKit 的「打开链接」，或把链接拖到读信区上，
远程页面就在 web view 里载入：没有我们的 CSP，图片、CSS 全加载，页号对不上、高度不更新，直到换一封。
「下载链接文件」也没人管。写信窗口的引文用同一个 `web_view()`，一样。

## 决策

- 页面里拦（`BRIDGE`）：链接上的 `contextmenu` 不显示 WebKit 的菜单（连带「复制链接」，算代价；别处的菜单、
  复制照旧）；`dragover` / `drop` 一律 `preventDefault`，从外面拖进来的链接也不载入。没拦 `dragstart`：
  拖放已经不收，往外拖链接（拖到浏览器）无害。
- 页面看不到的兜底：`on_load_start` / `on_load` 报的 URL 不是 `about:blank`（或空）就把当前页面再载一遍。
  新的载入会取代还没 commit 的远程导航，已经载入的也被换掉；页号不变。同样的 HTML nsh 不会重载，所以在后面
  交替加一个换行。`StaysOnPage` 在重载完成前不再触发，免得 URL 滞后时来回重载。
- 兜底不打开链接：分不清是不是用户发起的，万一是页面自己跳转，会反复开浏览器。
- 没用 `ElementRef::stop_loading` 当主路径：命令异步到达，远程页面可能已经 commit，重载仍然要留，两套机制并存；
  重载走同一条路径，时机一样。
- `web_view` 的页面参数改为 `Fn() -> Rc<str>`：两个调用方手上本来就是 `Rc<str>`，web view 里只复制一次，
  加换行时一并留出空间，不再多一次整页的重新分配。
- 根本的修法在上游：nsh 让 Rust 应用设 web view 的导航策略，在导航开始前就拒掉（文档里标了 ⚠️）。

## 方案

- `catmail-frontend/src/html/page.rs`：`BRIDGE` 加 `link(e)`（`click` 与 `contextmenu` 共用）、`contextmenu`、
  `dragover` / `drop` 的监听。
- `catmail-frontend/src/html/mod.rs`：`StaysOnPage`。
- `catmail-gui/src/web.rs`：`web_view` 接上两个事件和换行切换；`reader/view.rs`、`composer/quote.rs` 改传 `Rc<str>`。
- `docs/architecture/gui.md`：「不离开页面」一条。
- 测试：`a_page_left_is_loaded_again_once`（先报开始、远程先载完、URL 滞后、只在载完时才发现）、
  `nothing_but_a_click_leads_the_page_to_a_link`。
- 未在 app 里实际验证：右键链接没有菜单、从 Safari 拖链接进来不载入、左键仍交给浏览器、选中文字右键仍有复制；
  兜底的重载也没在真机触发过。
