# 读信区只用 Blitz 显示 HTML

- 日期：2026-10-22
- 类型：重构

## 背景

接入 Blitz 时保留了 GPUI Kit 的 `TextView` 作为可切换的备选，因为 Blitz 页面当时不能选择文字；
另有 Text / HTML 切换，默认显示纯文本。页面能选择、复制文字之后，这两个切换都不再需要：GPUI Kit
排不了多数营销邮件，默认纯文本也让 Blitz 的排版要多点一次才看得到。

## 决策

- 有 HTML 部分就用 Blitz 排成页面，即使也有纯文本部分；只有纯文本时仍用 `SelectableText`，不把纯
  文本包成 HTML 交给 Blitz（没有必要多开一个渲染线程）。Blitz 排不好的邮件用"在浏览器打开"。
- 只为 `TextView` 存在的代码一并删除：proto 的 `mime/layout.rs`（`unwrap_layout_tables`，core 的
  re-export）、`Images::source` / `not_shown`、`OpenMessage` 里展开后的 HTML 副本。`Images` 不再存
  `gpui::Image`（只为交给 GPUI，还要逐字节算哈希），改存 MIME 类型和 `Bytes`；按 URL 的缓存也去掉
  （文本视图每帧都问，Blitz 只在打开文档时问）。有 HTML 时不保留纯文本、没有 HTML 时不取内嵌图片：
  都不会显示。
- `catmail-render` 的 `Engine` 枚举保留：它是渲染 crate 的接口，与读信区的切换无关。
- 腾出的位置放 Reply、Reply all、Forward（从顶部工具栏移来），离正文更近；顶部工具栏只留归档、删除、
  移动和"⋯"。按钮在 `Reader` 里，命令在 `Mailbox` 上：按钮经 `Mailbox` 的焦点句柄派发现有的
  `Reply` / `ReplyAll` / `Forward` action（按钮、菜单、快捷键走同一个 action）。不用
  `window.dispatch_action`：焦点不在邮箱里时它到不了 `Mailbox` 的 handler；也不另加事件类型。

## 方案

- `reader/mod.rs`：删去 `BodyFormat`、`HtmlEngine` 和对应的选择状态；`show_page` 只看打开的邮件
  有没有 HTML。`reader/view.rs`：格式栏换成 Reply、Reply all、Forward、"Show remote images"和
  "Open in browser"，正文是页面、纯文本或"没有正文"。`reader/message.rs`：`html` 即清洗后的 HTML（原 `browser_html`）。
- 文档：`docs/architecture/gui.md` 删去"展开布局表格"一节，更新读信、图片和 HTML 引擎的描述；
  README、`bodies.md` 同步。
- `mailbox/panes.rs` 去掉三个按钮，`mailbox/mod.rs` 把焦点句柄交给 `Reader`。
- 验证：GUI 测试 `switches_the_html_between_engines` 换成 `shows_html_as_a_page_and_text_without_one`
  （同时有纯文本的邮件也直接画出页面，只有纯文本的邮件没有页面）；其余点"format-html"的测试和截图
  测试去掉这一步。写信测试点的 "reply" 现在是读信区的按钮，覆盖派发 action 这条路；截图确认按钮排在
  "Show remote images" 左边。
