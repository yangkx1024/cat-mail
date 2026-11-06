# 隐藏读信区 web view 自己的滚动条

- 日期：2026-11-06
- 类型：修复

## 背景

macOS 上读长邮件时，读信区右侧出现两个滚动条：外层滚动区的和 WKWebView 自己的。web view 的高度跟着页面报的
高度走，本不该滚动；但 nsh 在 macOS 上的 `scrollable(false)` 只把滚轮交给外层（`passes_wheel`），不隐藏页面的
overflow（Linux 上注入 `html, body { overflow: hidden !important; }`）。页面报的是 `body.scrollHeight`，它是取整
后的整数，正文高度带小数时 web view 就矮了不到 1 px；body 的 `overflow-x: auto` 又传到 viewport，宽邮件的横向
滚动条（“始终显示滚动条”或接了鼠标时）占掉高度，纵向也就溢出。系统设置为始终显示滚动条时最明显。

## 决策

- 两处一起改：只隐藏 overflow 而测高仍偏小，正文底部会被切掉几个像素；只改测高，取整之外的溢出仍会出滚动条。
- 只隐藏 `html` 的 overflow，不连 body 一起：根元素的 overflow 不再是 visible，body 的 `overflow-x: auto` 就留在
  body 上，宽邮件在 body 里横向滚动，横向滚动条算在 body 的盒子里。
- 测高用根元素的 `getBoundingClientRect()`（加上它的外边距）向上取整：包含 body 的外边距（邮件自己的样式可能
  设）和 body 的横向滚动条。放弃 `documentElement.scrollHeight`：它不小于 web view 当前的高度，web view 是所有
  邮件共用的，换到短邮件时缩不回去。

## 方案

- `crates/catmail-frontend/src/html/page.rs`：`BASE_STYLE` 加 `html { overflow: hidden !important; }`；`BRIDGE`
  的 `report` 改量根元素的盒子，`ResizeObserver` 观察根元素的 border-box。
- `docs/architecture/gui.md` 的“高度”一条写明原因。
- 测试：`the_page_itself_never_scrolls`（页面隐藏根元素的 overflow，测高不用 `scrollHeight`）。界面上的效果需在
  macOS 上手动看：长邮件、比读信区宽的邮件，系统设置“显示滚动条”为“始终”。
