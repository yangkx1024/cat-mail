# 读信区的横向滚动条显示在读信区底部

- 日期：2026-11-18
- 类型：修复

## 背景

#355：比读信区宽的邮件，要纵向滚到最底部才看得到水平滚动条。读信区是 nsh 的纵向滚动区，里面放标题、头部、
和页面一样高的 web view、附件；web view 自己不滚，宽邮件靠 `<body style="overflow-x: auto">` 横向滚动，
body 的横向滚动条就画在 body 底部，也就是整封邮件的末尾。纵向指示条属于外层滚动区，钉在视口右边，所以一直看得到。

## 决策

- 不让外层两个方向一起滚：nsh 的滚动区只沿一个方向，两个方向都滚只有 `zoomable` 且后端支持缩放
  （`ScrollState::can_zoom`）时才行，macOS 和 Linux 后端都不支持。
- 不用 nsh-base 的 `Scrollbar`：它跟的是 nsh 的滚动区或列表（`watch_scroll` / `scroll_to`），这里滚的是页面里的 body。
  拖动和淡出的写法照它来。
- 页面藏起 body 自己的横向滚动条、回报横向滚动，读信区在滚动区底部自己画一条，样子和来去都照 nsh 的纵向指示条
  （macOS 6pt、Linux 3pt，黑 45% / 白 55%，停 700 ms 后 300 ms 淡出）。外层纵向滚动时它也出现，和一个视图的两条
  指示条一起出现一样，宽邮件不滚横向也看得出能横向滚。
- 鼠标在读信区上移动也让它出现，和纵向指示条一样：macOS 上移到读信区任何地方都出现（系统设置的“显示滚动条”
  为“滚动时”就不出现，nsh 读的是同一个 `AppleShowScrollBars`），Linux 上只在移到滑块上时出现（nsh 的
  `grabs_thumb`：离底边 12pt、滑块两边各 4pt 内）；按着键拖动不算。
- 隐藏着时不接鼠标，不挡页面底部的点击。

## 方案

- `catmail-frontend/src/html/page.rs`：body 的 `style` 属性加 `scrollbar-width: none !important`，`BASE_STYLE` 加
  `body::-webkit-scrollbar { display: none !important; }`（不认 `scrollbar-width` 的 WebKit）；`BRIDGE` 在 body
  滚动、body 和它的子元素尺寸变化、图片到了时，有变化才报（尺寸变化和图片一帧并成一次：读宽度要排版）
  `x:<页号>:<偏移>:<clientWidth>:<scrollWidth>`，偏移从左边算起：邮件的样式表能让 body 从右往左排
  （`direction: rtl`），那时 `scrollLeft` 在右端是 0、往左是负数，要换算（PR 上 Codex 指出的）；
  `FromPage::Sideways` 解析它，解析改成按段数匹配，多出、缺少、非有限的数都不认。高度不再含横向滚动条。
- `catmail-frontend/src/html/sideways.rs`：`Sideways`，滑块的长度和位置（最短 32pt）、拖动和点击滑块旁边对应的偏移，纯函数加测试。
- `catmail-gui/src/web/sideways.rs`：`web::Sideways`，每个 web view 一个：收页面的回报，画滚动条，拖动、点击时注入
  `html::scroll_sideways` 的脚本，同样按方向换算，不依赖 `BRIDGE` 里的函数（页面 JS 关着也照样执行：
  macOS 关的是 `allowsContentJavaScript`，Linux 关的是 `enable-javascript-markup`）。拖动中页面晚到的位置不跟，滑块跟着指针。
- `web::web_view` 多一个 `Sideways` 参数；读信区（`reader/view.rs`）和写信窗口的引文（`composer/quote.rs`）
  把滚动区交给 `Sideways::around`，由它放进同一个 view 并挂上滚动区的 `on_scroll`、这个 view 的
  `on_mouse_move`，让滚动条出现，调用处不用各接一遍。macOS 读 `AppleShowScrollBars` 要 `objc2-foundation` 的 `NSString`、`NSUserDefaults` 两个 feature。
- 文档：`docs/architecture/gui.md` 的脚本、高度两段和新加的「横向滚动条」，ROADMAP 记一条 ⚠️。
- 验证：`cargo fmt`、`clippy -D warnings`、全量测试；新增 `html::sideways` 的测试和 `page.rs` 里页面藏起滚动条、
  回报 `x`、解析 `x` 的测试。
