# catmail-nsh：macOS 状态栏图标，窗口外框对齐 gpui 版

- 日期：2026-11-06
- 类型：功能

## 背景

catmail-nsh 在 macOS 上没有状态栏图标（关窗常驻后只能点 Dock 回来），窗口也是系统标题栏：只写着窗口名，
占一行。gpui 版在 macOS 上主窗口是一体式（红绿灯叠在侧栏左上角、各栏顶部那一行里，这一行拖动窗口），
写信窗口是按主题颜色画的标题栏。

## 决策

- **状态栏**：用 frontend 的 `tray::macos`（见同日的 `share-the-macos-status-item-through-catmail-frontend`）。
  `tray.rs` 拆成共用的 `tray/mod.rs`（计数、画、事件）和 `tray/linux.rs`、`tray/macos.rs`（各自的 `Item`、
  图标的样子、外观跟随，以及在后台线程上把 resvg 的像素转成各自 `Item` 要的格式），与 gpui 版 `tray/mod.rs`
  的结构相同。macOS 的模板图不变，画一次之后只更新数字；
  模板图里没有文字，所以只有 SVG 里有 `<text>` 时才加载系统字体。在主线程上跑用 `shell::spawn`（GTK 的 /
  dispatch queue 的），不再直接用 glib。
- **外框**：用 nsh 自己的 `set_title_bar(TitleBarStyle::Hidden, height)` 和 `window_drag`，不自己摆红绿灯：
  nsh 给窗口装一个空的 NSToolbar，AppKit 在缩放、全屏时自己摆按钮。新模块 `chrome.rs` 对应 gpui 的
  `layout::Frame`：
  - 主窗口要 48 pt，AppKit 选 unified 工具栏，红绿灯居中在 52 pt 的行里。nsh 的 `title_bar()` 在 macOS 27
    上报 66 pt（它量的是按钮外面那个更高的视图），按它排各栏顶部会比按钮低 7 pt，所以行高固定 52 pt，
    `title_bar()` 只用来判断有没有按钮（全屏时为 0，行高回到 48）。这是 nsh 的问题（`title_bar_view_height`
    量的是按钮的 superview），已报到上游 longbridge/nsh#1068（用 nsh 自己的 `run_app` 复现），修好后改回读 `title_bar().top`。
  - 顶部那一行：侧栏空出一行（`move_row`），Compose 在它下面；中栏的同步 + 搜索一行改成这一行的高度
    并垂直居中（原来上边距 12 + 36，与右栏工具栏不对齐，gpui 版是同高的一行）；读信、草稿、发件箱的工具栏
    同高（三个工具栏都用 `chrome::toolbar`，搜索行用 `chrome::move_row`，高度和拖动只在 `chrome.rs` 一处）。
    这些行都 `window_drag(true)`：按钮、输入框照常可点，空白处拖动、双击按系统设置缩放。
  - 登录页、引擎打不开的页、等待引擎的空白页没有自己的顶栏：上面放一行 `move_row`（`chrome::page`）。
    考虑过把拖动行盖在页面上面，但会挡住滚到顶部的输入框；也考虑过整页可拖，和 gpui 版不一致。
  - 写信窗口要 34 pt（GPUI Kit 的 `TitleBar` 高度），AppKit 选 compact，按 `title_bar()` 报的高度画一行：
    侧栏颜色、底部边线、Subject 居中（两边各留按钮的宽度），可拖动。
  - Linux 不调 `set_title_bar`，仍是系统标题栏，行高 48 不变。

## 方案

- 新增 `chrome.rs`、`tray/mod.rs`、`tray/macos.rs`；`tray.rs` → `tray/linux.rs`。`shell/appkit.rs` 的
  `engine_opened` 启动托盘；`shell/mod.rs` 导出 `spawn`，去掉托盘相关函数上"只在 Linux"的 `expect(dead_code)`。
  resvg 改为两个平台都依赖。
- `app.rs`（`chrome::unified`、`chrome::page`）、`sidebar.rs`、`list.rs`、`reader.rs`、`draft.rs`、`outbox.rs`、
  `composer.rs`；ROADMAP 的 P9、P10。
- 验证：macOS 上 fmt、`clippy --workspace --all-targets -D warnings`、全部测试通过（新测试
  `the_menu_bar_template_is_drawn`）。在本机临时接了 testkit 的假服务器（未提交）启动：截图量得红绿灯与
  搜索行的中心都在约 26 pt，Compose 从 52 pt 开始；写信窗口的标题栏与按钮对齐、标题居中。状态栏项在
  macOS 27 的窗口列表里看不到，没有截图确认；拖动、双击没法自动测。Linux 的 `tray/linux.rs` 本机编译不到，
  靠 CI。
