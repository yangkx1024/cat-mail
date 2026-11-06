# nsh 版侧栏的子文件夹折叠，行的样式和结构对齐 GPUI Kit

- 日期：2026-11-06
- 类型：功能

## 背景

- gpui 版里有子文件夹的行可以收起，nsh 版不行。
- nsh 版侧栏和设置导航的选中样式是改字重；gpui 版（`layout::sidebar_row`）只改颜色，图标换成填充的。间距也不一样。
- 原来的侧栏是一份扁平的行列表，嵌套全靠每行的深度。

## 决策

- 侧栏照 gpui 版的结构来建：
  - All Inboxes 一组；每个账户一组，地址在上、地点在下；标签单独一节。
  - 子文件夹放在 GPUI Kit submenu 那样的容器里：`ml 14`、左边框、`pl 10`、`py 2`、`gap 4`。
  - 竖线就是容器的左边框，间距都用容器的 gap。
  - 最初在扁平列表上做过：每行带上方间距和"结束了几层"，竖线按坐标一根根画。规则散在几处，还要一个测试
    专门核对坐标计算，所以改成了嵌套。
- 文件夹的折叠记在 `FolderFolds` 里，按行的 key；默认都展开，同 gpui 版。账户的折叠另记（`Folds`），因为默认值
  不同：只有一个账户时才默认展开。
- 侧栏的地点和设置导航用同一种行（`sidebar::menu_item`）：
  - 行高 28，`px 8`，`gap 8`，圆角 6；
  - 图标和文字一样大（14）；
  - 选中时用 `sidebar_accent` 底，图标和文字换成主色，图标换成填充的；
  - 悬停时是 `sidebar_accent` 的 0.8。
- 悬停改用 nsh 的 `hover` 样式，给它闭包：颜色跟着主题切换和选中变化，在侧栏和设置导航上验证过。
  - 11-05 的记录说 `hover` 只取构建时的颜色，那是因为当时传的是固定颜色。
  - 其余按钮（`icon_button`、`text_button`、设置里的按钮和主题卡片、"Move to"的行、附件）也一起去掉了鼠标进出的
    信号。这些只看了代码，没有逐个看界面。

## 方案

- `crates/catmail-nsh/src/sidebar.rs`：
  - `Group`、`Heading`、`Item`、`groups`、`places`、`folder_item`；
  - `group`、`heading`、`menu`、`menu_entry`、`place_row`、`menu_item`；
  - 测试 `a_folded_folder_hides_its_subfolders`。
- `crates/catmail-nsh/src/icons.rs`：
  - 填充的图标和 `Icon::solid`；
  - 文件夹图标的两种样式出自同一张表（`folder_icon!`）；
  - 测试 `a_selected_row_has_its_icon_filled`。
- `crates/catmail-nsh/src/settings.rs`：导航宽 160，用 `menu_item`。
- `commands.rs`、`settings.rs`、`reader.rs`：改用 `hover` 样式。

验证：
- 和 gpui 版截图对比侧栏和设置导航；
- 手动检查折叠、竖线、悬停、选中时悬停、切换外观后悬停；
- catmail-nsh 的 clippy `-D warnings` 和测试通过。
