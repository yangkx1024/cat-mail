# 邮件的 CSS 不再把阅读页的高度钉在 web view 的高度上

- 日期：2026-11-09
- 类型：修复

## 背景

macOS 上刚打开应用就选中 DBS 的营销邮件（"Your documents are now digital with eDocuments"），正文只显示几行；
换一封再换回来，正文高度看起来正常，但 web view 里出现纵向滚动条，鼠标悬停还会再冒出一个。

原因是邮件自己的样式 `html, body { height: 100% !important; }`：根元素和 body 和 web view 一样高。BRIDGE 量的是
根元素的盒子，报回去的就是 web view 当前的高度，web view 永远长不高（开窗时是空白页的高度，只露出几行；
换过邮件后停在上一封的高度）。放不下的内容在 body 里：body 的 `overflow-x: auto` 让 `overflow-y` 也算成
`auto`，于是 body 自己纵向滚动，出现第二、第三个滚动条。我们的 `html { overflow: hidden !important; }` 也同理
可能被邮件后面同样加 `!important` 的规则盖掉。

## 决策

- 页面的尺寸规则改写在 `<html>`、`<body>` 的 `style` 属性里，带 `!important`：`style` 属性里的 `!important` 比
  任何样式表的都优先，和选择器的特异度、先后都无关，邮件的样式表盖不掉（清洗后的邮件不会带 `<html>`/`<body>`
  标签，页面脚本也关着）。
- 考虑过的做法：提高我们 `<style>` 里选择器的特异度——邮件可以用更高的特异度赢回去；用 `@layer`（层里的
  `!important` 胜过不在层里的）——也能行，但 `style` 属性更简单，不依赖 WebKit 版本；在清洗时去掉作用于
  `html`/`body` 的 `height`——只治这一种写法，`min-height: 100%` 等还会出同样的问题。
- 三条高度（`height: auto`、`min-height: 0`、`max-height: none`）和 `position: static` 是一个常量 `SIZE`，根元素和
  body 共用（绝对定位的 body 脱离文档流，根元素量出来接近 0；带 `top: 0; bottom: 0` 时又被钉在 web view 的高度。
  代价是邮件里以 body 为参照的绝对定位元素改以视口为参照，body 在左上角、没有外边距，只有贴底的会挪位置）；根元素再加
  `overflow: hidden`，body 再加 `overflow-x: auto; overflow-y: hidden`（原来样式表里的 `overflow-x: auto` 也挪进来，
  body 怎么滚只在一处），body 只横向滚动宽邮件，绝不纵向滚。
- 不改测高方式：量 body 的内容（子元素的盒子、`scrollHeight`）要处理取整、外边距合并、浮动和绝对定位，也解决不了
  `100vh`。
- 没处理：邮件里元素用 `100vh` 定高时，vh 就是 web view 的高度，同样会互相撑住（按视口高度的 `@media` 查询同理）。
  要在清洗时丢掉带视口高度单位的声明，并升 `CONTENT_VERSION` 重建已存的正文，还会改变这类邮件的显示；没见到实例，
  先不管。

## 方案

- `crates/catmail-frontend/src/html/page.rs`：新增 `SIZE`，和 overflow 一起写到 `<html>`、`<body>` 的 `style` 属性；
  `BASE_STYLE` 去掉 `html { overflow: hidden }` 和 body 的 `overflow-x`。
- 测试：`the_page_itself_never_scrolls` 改为检查 `<html>`、`<body>` 带着这两个 style 属性，且都有 `position: static`
  （改之前的代码上失败）。
- `docs/architecture/gui.md` 的"高度"一条写明原因。
- 沙箱里起不了 WebKit，渲染效果需在 macOS 上手动看：开窗后直接点开这封 DBS 邮件，正文完整、只有外层一个滚动条。
