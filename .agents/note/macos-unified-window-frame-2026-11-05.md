# macOS 去掉系统标题栏，主窗口一体式

- 日期：2026-11-05
- 类型：功能

## 背景

#235：macOS 上窗口顶部的系统标题栏只写着 "catmail"，占一行又不提供任何东西。去掉它，保留红绿灯。

## 决策

- 主窗口用一体式（issue 里倾向的方案，像 Mail.app）：没有单独的标题栏，侧栏延伸到顶部，红绿灯叠在侧栏
  左上角，各栏顶部的工具栏兼作拖动区域。备选的 gpui-kit `TitleBar` 会留一条 34px 的自绘标题栏，没采用。
- 写信窗口保留标题栏，但要与主窗口的主题一致（用户的要求），改用 gpui-kit 的 `TitleBar`，按主题着色、标题
  居中。系统标题栏借 `App::set_window_appearance` 最多跟上主题的浅色 / 深色，配不上导入主题的颜色。
- 设置是主窗口里的对话框，不是单独的窗口，不用处理。Linux、Windows 不变，仍用系统标题栏。
- 拖动和双击照 `TitleBar` 的做法：`app_owns_titlebar_drag`，按下记一笔、移动时 `start_window_move`，
  双击 `titlebar_double_click`。区分"按在控件上"不另做命中测试：按钮、输入框、复选框按下时都
  `prevent_default`，拖动区放在内容后面，冒泡到它时看 `default_prevented` 就知道。
- 拖动区只有一条，横在窗口顶部（`layout::movable`），而不是让每个工具栏、页面各自可拖动：起初是逐处包
  （`toolbar`、中栏顶部、侧栏、登录页、空白页），简化审查指出新页面容易漏。它要放进 `Mailbox` 的根里：
  根 `track_focus`，按下时为拿焦点也 `prevent_default`，放在外面就收不到（测试先发现）。按下状态不
  `notify`：没有东西按它渲染，`Entity::write` 会让整个窗口重绘。
- 顶部一行就是工具栏的高度（3 rem）：中栏的同步、搜索一行改成同样高，与右栏工具栏、红绿灯对齐（原来
  是 `p_3` 加按钮高度，比工具栏低 4px）。这一行随文字大小变，红绿灯在 `MainWindow` 渲染时按 rem 重摆，
  打开窗口时按当前设置的文字大小放，不先出现在默认位置。

## 方案

- `layout.rs`：`Frame`（`Unified` / `TitleBar`）进 `window_options`；`bar_height`、`has_window_buttons`、
  `place_window_buttons`、`movable`（内含私有的 `MoveRow`）。初始的红绿灯位置用主题的 `font_size`，与
  `Root` 设的 rem 同源。
- `main.rs`：主窗口 `Frame::Unified`，`MainWindow` 记着红绿灯的位置；空白页、失败页包 `movable`。
- `mailbox/`：根里包 `movable`；侧栏顶部留一行（全屏时不留）；中栏同步、搜索一行与工具栏同高。
- `composer/`：`Frame::TitleBar`，macOS 上顶部渲染 `TitleBar`，标题是信的 Subject。
- 文档：`docs/architecture/gui.md` 新增"窗口外框（macOS）"。
- 验证：新增 `mailbox/tests/frame.rs`（只在 macOS 编译）：从工具栏按钮、搜索框拖动不移动窗口（去掉
  `default_prevented` 判断时失败）；从工具栏空白处、侧栏顶部拖动会移动窗口（测试平台的
  `start_window_move` 是 `unimplemented!()`，以它的 panic 判断）。macOS 上 clippy、fmt、GUI 测试通过
  （`instance` 的 6 个测试因沙箱不能建 unix socket 失败，改动前同样失败）。真机上的布局还要人看。
