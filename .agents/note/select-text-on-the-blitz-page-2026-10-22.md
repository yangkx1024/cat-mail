# 在 Blitz 排版的页面上选择文字

- 日期：2026-10-22
- 类型：功能

## 背景

Blitz 把正文画成像素分块，不能选择和复制文字，要复制只能切到 Text 或 GPUI Kit。补上选择后，
GPUI Kit 的 HTML 渲染就只剩排版不了时的备选，可以考虑去掉。

## 决策

- 加入 gpui-base 的窗口级选区（`TextSelectionHandle`），不自己处理拖动：拖选、点别处清除、Ctrl+C、
  跨参与者的选区、拖到边缘自动滚动都与 `TextView` 一致，不用重写。
- 引擎在渲染线程上按两端位置找出文字和高亮矩形；复制回调是同步的，所以 `PageView` 存下最近一次的
  文字，不在 UI 线程上等渲染线程。
- 不用 Blitz 自己的选区：它以固定浅蓝色画进分块（深色主题看不清），每次拖动还要重画屏幕上的分块。
  改为 GPUI 在分块之上画主题的半透明 `selection` 色。
- 位置靠几何（按布局树遍历 inline root，段落内容框、同行最近、上方最近段落末尾），不靠 Blitz 的
  `find_text_position`：后者在段落之间返回空，拖过空白时选区会跳。
- 双击选词、三击选段落（`hard_line_from_point`，以 `<br>` 和块为界）：窗口选区要靠参与者的文字 run
  找词，页面没有，所以作为本地选区自己处理。
- 引擎请求 `link_at` 换成 `hit`，同时回答是否在文字上，用于 I 形光标。选择只有一个请求
  `select(Span)`：两端之间，或某点周围的词、段落。
- 行内块（inline-block）是另一个段落，但按它在所在行里的位置读：所在段落的文字在 Parley inline box
  的位置切成片段，行内块的文字读在两段之间、不换行（Codex review 指出原先全选得到
  "before  after\nINNER"）。恰在行内块处的偏移分不出块前块后：拖动按点在块中线哪一侧定，选词不带
  边上的块、选段落带上；块在行尾时块后留一个空片段，否则拖过整行仍漏掉它（Codex 第二轮指出）。高亮同理：Parley 把 inline box
  的宽度算进它后面的字、整行选中时覆盖整行，所在段落的矩形要挖掉有文字的块，否则块后开始的选区会盖住
  块、全选时块被叠画两遍（Codex 第三轮指出）。
- 不画的不选：按 Blitz 绘制的规则，`visibility` 不是 visible 或 `opacity` 为 0 的元素整棵子树跳过
  （邮件里隐藏的预览文字不该被复制）。要直接依赖 `stylo`（以 Blitz 用的名字 `style`）才能读计算样式。
- 排版按序号区分而不是按尺寸：图片落在固定高度的盒子里，尺寸不变而文字移动，选区要重新问。新排版
  还没显示时不问，否则引擎按新排版回答的结果会被当成旧排版的画上去。
- 已知局限：选区记的是坐标，重排（换宽度、图片到了）后按同样的坐标重新找，可能落到别的文字上；
  与 GPUI Kit 的文字选区一致。要稳定可以改记段落和偏移。
- 右键菜单有 "Copy" 和 "Select all"，没有选区时 "Copy" 置灰。全选是 gpui-base 的 `SelectAll`
  action，页面获得焦点时 Ctrl+A 也触发（`READER_PAGE` 键上下文），作为页面的本地选区。
- "Copy" 菜单项不派发 `Copy` action：选区层对任何左键按下（包括按在
  菜单上）都先清空窗口选区，action 到 Root 时已无可复制。改为菜单打开时记下窗口选中的文字（与 Ctrl+C 一致），点击时直接写
  剪贴板；复制后选区消失是正常交互，不放回。
- 菜单项点击会阻止松开事件传到选区层，它要到下一次松开才复位，期间那次按下不清空本地选区（全选后
  单击清不掉）。页面在窗口上监听 capture 阶段的左键按下，按在任何地方都清掉自己的本地选区。这是 gpui-kit 0.8.0 的问题。

## 方案

- `catmail-render`：`selection.rs`（`Text`：段落列表按排版缓存，`select` / `select_around`），
  `lib.rs` 新增 `Rect`、`Hit`、`SelectionEnd`、`Unit`、`Selection`，`worker.rs` 新增对应请求；直接
  依赖 `parley` 和 `stylo`（Blitz 用的同一版本）。
- `catmail-gui/src/reader/page.rs`：注册为选区参与者（整页为文字区域）、订阅选区变化问引擎、重排后
  重新问、画高亮、双击 / 三击、拖选结束在链接上不打开链接、选区开始时取得焦点以便复制、右键菜单和全选（`actions.rs` 绑定 Ctrl+A）。
- 文档：`docs/architecture/gui.md` 的"选择文字"，README。
- 验证：`catmail-render` 单元测试（两处之间的文字、段落间空白、行尾和行首之外、页面上下、表格、
  选词和段落、行内块按位置读、隐藏段落不选）；GUI 测试 `a_selection_follows_text_an_image_moves`（图片在固定高度盒子里推动文字后选区跟着走）和 `text_is_selected_and_copied`（拖选后 Ctrl+C、不打开链接、双击、三击、
  点击清除、点击链接仍会打开、右键菜单复制和全选、Ctrl+A、全选后单击清除）；截图测试新增 `html-newsletter-selected` 和
  `html-newsletter-context-menu`，人工看过高亮和菜单。
- GUI 测试里的点击只落在行首、按行区分，不按词宽取横坐标：字宽随机器上的字体而变，CI 的 Ubuntu 用
  DejaVu Sans，"gamma " 比本机宽，x=80 落在链接之前，测试超时。用只含 DejaVu 的 `FONTCONFIG_FILE`
  在本机复现并验证。
