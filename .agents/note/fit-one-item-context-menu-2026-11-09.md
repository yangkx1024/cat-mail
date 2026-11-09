# 修复 Linux 上只有一项的右键菜单下方多出空白

- 日期：2026-11-09
- 类型：修复

## 背景

Linux（GTK）上只有一项的右键菜单（如账户行上只有 New folder）下方比上方空出一截，看起来不对称。两项及以上的菜单正常。

nsh 的菜单是 `gtk::PopoverMenu`，菜单项放在一个 `ScrolledWindow` 里。滚动窗口的高度不小于竖向滚动条的最小高度（不显示时也算）：GTK 主题给滑块 40 px，加上下边距共 44 px；而 nsh 样式下一项菜单的内容只有 40 px（6 + 28 + 6），于是被撑高 4 px，多出的都在下方。

## 决策

- 根因在 nsh 的菜单样式表（`nsh-platform-linux` 的 `menu_style.rs`），修复提给上游（longbridge/nsh#1359）。
- nsh 没有让应用改它菜单样式的接口，但导出了 `gtk4`，shell 本来就直接用它。为了不等上游合并和升级 nsh，shell 先自己加一条样式，标 ⚠️，nsh 有了这条就删。
- 只缩短菜单里竖向滚动条的滑块（16 px），不动菜单项和内边距；菜单超出屏幕要滚动时滚动条照常出现。

## 方案

- `crates/catmail-gui/src/shell/gtk.rs`：`MENU_CSS` 和 `style_menus`，启动时以 `STYLE_PROVIDER_PRIORITY_APPLICATION + 1` 加到 display 上，排在 nsh 的菜单样式之后（nsh 换主题色时原地重载自己的 provider，不会盖掉这条）。
- `crates/catmail-gui/ROADMAP.md`：记 ⚠️。
- 测试 `a_menu_of_one_item_fits_it`：一项菜单的滚动窗口高度等于菜单项的高度。需要显示器，CI 上没有所以不跑；本地在无头 weston 下跑，去掉修复时失败（44 ≠ 40）。另用无头 weston 截图量过：修复后 1、2、3 项的菜单上下都是 6 px。
