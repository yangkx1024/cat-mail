# 侧栏折叠和列表增删的动画

- 日期：2026-11-25
- 类型：功能

## 背景

侧栏收起、展开账户和子文件夹时内容直接出现、消失，箭头直接换图标；列表来了新邮件、移走或删除一封时，
行直接跳到新位置。ROADMAP 里"折叠的动画"一直没做。

## 决策

- 侧栏用 nsh-base 的 `Collapsible`（`reveal`）：内容放在裁剪的区域里，高度弹簧式收拢、展开（默认 180 ms 响应、
  临界阻尼，不过冲），下面的行跟着移；内容等区域合上后才移除。没自己拿 `ElementRef::animate_layout` 实现，
  因为 `Collapsible` 已经处理好了"合上后再移除"和减弱动态效果。
  - 这要求模型在折叠时也给出地点和子文件夹：`sidebar_groups` 不再在折叠时返回空的 `items`、`labels`、`children`，
    由 `Heading::open`、`Item::open` 说是否显示。测试里的 `shape` 只列显示的部分，原有期望不变。
  - 箭头不再在 ChevronRight 和 ChevronDown 之间换，而是一个 ChevronRight 转 90°（`motion::chevron`，240 ms
    ease-in-out，样式的 `transition_transform`，减弱动态效果时平台直接跳过），账户标题、文件夹和写信窗口引文的
    折叠按钮（`buttons::fold_button`）都用它。
- 列表用全局的 `animate_layout`（nsh 的列表只在整次提交的布局动画下让行滑动，见 nsh-core `list_motion.rs`；
  `ElementRef::animate_layout` 不驱动列表的行）：重读已显示的行前调 `motion::glide_rows`，留下的行 240 ms
  滑到新位置，新来的 240 ms 淡入，离开的 180 ms。
  - 只在 `Rows::Replace` 且列表原来有行时做：换来源（新的 `Listing` 是空的）和翻页（`Append`）不做。
  - 读信区随之打开或关上时，右栏的动画在同一轮里后要求，取代它（nsh 一次提交只用最后一个根布局动画），
    `glides` 不必再推算右栏开没开。
  - 日期标题原来按行号作 key，上面多一行它的 key 就变了，会被当成删掉一个、新建一个而淡入淡出。改成按日期和
    这天的第几段（`LineKey::Day(Option<Date>, usize)`），跟着行滑动。
- 减弱动态效果时都没有动画（`Collapsible` 自己判断；`glide_rows`、`chevron` 看 `reduce_motion()`）。

## 方案

- `crates/catmail-frontend/src/mailbox/sidebar.rs`：折叠时保留地点和子文件夹；测试补上"折叠后仍保留"。
- `crates/catmail-frontend/src/list/model.rs`：`LineKey::Day` 按日期和段号；测试 `a_page_more_adds_lines_to_the_last_day_and_after_it`
  补上：上面来了一行时标题的 key 不变，同一天第二段的标题 key 不同。
- `crates/catmail-gui/src/motion.rs`（新）：时长、`glide_rows`、`chevron`。
- `crates/catmail-gui/src/buttons.rs`：`fold_button`。
- `crates/catmail-gui/src/mailbox/sidebar.rs`：分组和子文件夹放进 `collapsible().reveal(true)`，箭头用 `chevron`。
- `crates/catmail-gui/src/mailbox/list/mod.rs`：`show_loaded` 在应用行之前按 `glides` 决定是否 `glide_rows`。
- GUI 测试 `accounts_and_folders_fold_and_unfold`：新建 Projects 和其中的 2026，收起、展开文件夹和账户后内容
  消失、回来（无头测试开着减弱动态效果，只验证结果，不验证帧）。
- 动画本身要在运行中的应用里看：收起、展开账户和文件夹；新邮件到达、归档或删除一封。

## 追加：读信区打开、关上

- 原来读信区没打开时窄窄地压在列表左边（宽 256），打开时直接跳到列表右边；草稿、发件箱的右栏打开时才插入。
- 考虑过对整次提交 `animate_layout`：读信区的帧会从左边滑过来并一路变宽，WKWebView / WebKitGTK 每帧重排页面，
  而且草稿右栏是新插入的，只能淡入，放弃。也考虑过右栏淡入加平移（只动 opacity / transform）：打开还行，关上时
  列表立刻变宽盖住它，看不到动画，放弃。
- 现在是"幕布"：读信区和草稿、发件箱的右栏都放在打开时的位置（`left = 侧栏 + 列表宽`），压在列表下面；`open`
  变化时整次提交 `animate_layout`（`motion::glide_when`，240 ms ease-in-out，新出现的淡入），列表的边滑窄露出
  右栏、滑宽盖回。右栏的帧不变，web view 不重排。
  - 先试过只对列表这一栏 `ElementRef::animate_layout`：nsh 只在整次提交的布局动画下处理列表的行
    （`list_motion.rs`：行随列表自己的滚动留在原处、换给别的行的单元格直接到位），这一栏的行从一行变两行时
    可能错位，所以改成整次提交的。右栏不动以后，整次提交的动画也牵扯不到别的。
  - 选中那封滚到中间（`scroll_to_index`）变成平台的 `ScrollTo`，nsh 不补偿它，一下跳过去会让行先整体跳一下；
    有动画时改成动画滚动，和行的滑动一起走。
  - 右栏关着时宽度冻结在上次打开时的值（`on_layout` 记下打开时读信区的宽度），只跟着侧栏、列表的宽度挪位置：
    拖边、改窗口大小不会让看不见的 web view 重排页面（原来固定 256 宽就是为了这个）。关着期间窗口变了，下次
    打开时宽度变化也在动画里，重排几次。第一次打开前没有冻结的值，web view 是空的，跟着窗口不费事。
  - 关上时右栏留着上一次的（`shown` 记着最后打开的那种），否则草稿右栏会立刻消失，列表滑过一片空白。
  - 草稿、发件箱的右栏加了背景，压住下面的读信区；右栏盖住时对读屏隐藏（`accessibility_elements_hidden`）。
- 列表增删的 `glide_rows` 不在右栏开关的那次提交里做，两者不会叠加。
- `crates/catmail-gui/src/mailbox/panes.rs`、`motion.rs`、`mailbox/list/mod.rs`。GUI 测试全部通过（无头测试是减弱
  动态效果，不看帧）。

## 追加：动画期间隐藏滚动指示器（macOS）

- 现象：右栏打开、关上时，列表的滚动条另有一段移动的动画，和列表的边对不上。
- 原因（读 nsh 0.7.0 `nsh-platform-macos/src/scroll.rs`、`transition.rs`）：指示器是滚动视图上一层 overlay
  里的 `CALayer`，只在 `update_overlays` 里定位，而它只由滚动的每一帧和内容大小的变化调用；`transition.rs`
  逐帧写视图的 frame 时不调用它。所以提交时（内容变高）它直接按终点的大小摆好，之后只在滚动的帧里跟着当时的
  宽度挪，纵向跟着 nsh 自己的滚动曲线（300 ms ease-out）。不是隐式动画：提交和逐帧都在 `quiet()` 里。
- 考虑过换成 nsh-base 的 `Scrollbar`（普通视图，会跟着布局动画）：外观要自己仿原生、不再读系统的"显示滚动条"
  设置、thumb 位置经引擎、显示时挡住行最右边的点击，位置在提交时仍会跳，不划算。
- 现在：`motion::hide_indicators(ms)` 让列表（`.scrollbar(bool)`）和侧栏（`ScrollbarSetting`）的指示器在这段
  时间里关掉，和动画在同一次提交；关掉时 nsh 把它的透明度置 0，之后滚动或悬停才再出现。
  - 右栏开关：关 max(240 ms, nsh 动画滚动的 300 ms) 再多一帧。任何滚动（含程序发起的）都会让指示器出现，
    所以要等滚到中间的动画滚动结束。
  - 侧栏折叠：`motion::fold`（包一层 `Collapsible` 的 reveal）在 `open` 变化时关上弹簧收拢的时长；弹簧显式传
    `reveal_spring()`（与 nsh-base 默认相同，180 ms 响应），时长才可知。
  - 减弱动态效果时不隐藏。
- 根因已报给 nsh（longbridge/nsh#3188）：滚动视图的 frame 每次变化（含动画的每一帧）都应重新定位指示器。修了以后去掉这个绕开。
