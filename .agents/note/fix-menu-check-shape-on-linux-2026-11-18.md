# 修复 Linux 下拉菜单里的勾显示成大 "V"

- 日期：2026-11-18
- 类型：修复

## 背景

Linux（GTK）上所有下拉选择（设置里的层级、账户、文字大小，写信的 From 等 `popup_menu`）当前项前面的勾不是 ✓，而是一个又宽又大的 "V"，还把那一项的文字往右推，和其他项对不齐。

nsh 的菜单是 `gtk::PopoverMenu`，它的样式表（`nsh-platform-linux` 的 `menu_style.rs`）不用图标主题，而是把勾画成一个 4×8 px、只有右边和下边框的盒子，转 45°。但 check、radio、arrow 节点是 GTK 的内置图标，测量时不小于 `-gtk-icon-size`（默认 16 px，`min-width`/`min-height` 压不下去），盒子变成正方形，转 45° 就是对称的 "V"。子菜单的箭头同理。

## 决策

- 根因在 nsh 的菜单样式表，修复提给上游（longbridge/nsh#2383）：每个盒子把 `-gtk-icon-size` 设成自己的宽度。
- 和一项菜单的问题（`fit-one-item-context-menu-2026-11-09.md`）一样，nsh 没有改菜单样式的接口但导出了 `gtk4`，shell 先以 `STYLE_PROVIDER_PRIORITY_APPLICATION + 1` 加这几条，标 ⚠️，nsh 有了就删。

## 方案

- `crates/catmail-gui/src/shell/gtk.rs`：`MENU_CSS` 和 `style_menus`，启动时加到 display 上。
- `crates/catmail-gui/ROADMAP.md`：记 ⚠️。
- 测试 `a_menu_check_keeps_its_shape`：勾选项的 check 量出来是 4×8。需要显示器，CI 上没有所以不跑；本地去掉修复时失败（14×14）。另用无头 weston 渲染 nsh 的菜单样式截图对比过：修复后是 ✓，勾选项和未勾选项的文字对齐。
