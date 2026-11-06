# 托盘的计数/绘制状态机和应用图标移进 catmail-frontend

- 日期：2026-11-06
- 类型：重构

## 背景

#244：托盘的计数和绘制（`unread`、`dark`、`counting`、`drawing`、`drawn`）留在 gui 的 `tray/mod.rs`，"旧计数的图不能
最后落下"只在 gui 里、没有测试；应用图标的 SVG 在 catmail-gui 的 `assets/`，给它加角标的 `badged::svg` 和选哪张
图标的规则却在 frontend，只能由调用方把图标传进来。

## 决策

- `TrayModel`：`count` / `counted` 决定什么时候数、要不要画、要不要再数；`appearance` 说外观变了要不要重画；
  `draw` 给出要显示的数和要画的 SVG（和上次一样就不给），一次一个；`shown` 收下画好的那次，说要不要按现在的
  状态再画。托盘项（`Item`）和 resvg 仍在 gui：模型只管状态。
- `tray::icon(dark, unread)` 按平台给出 SVG 和尺寸：macOS 是菜单栏的模板图，其他平台是带角标的亮 / 暗应用图标。
- 图标资源整个搬到 `crates/catmail-frontend/assets/app-icon/`，不留在 gui 再跨 crate 读：gui 里已经没有别的地方用它，
  打包脚本（`packaging/macos/build-dmg.sh`）跟着改路径。
- 画成像素的测试（Linux 的角标数字、macOS 的模板图）还在 gui：它们测的是 gui 的 resvg 和字体，现在只有一份。

## 方案

- `catmail-frontend`：`tray/mod.rs`（`LIGHT_ICON`、`DARK_ICON`、`MENU_BAR_ICON`、`icon`）、`tray/model.rs`（`TrayModel`、
  `Drawing` 和测试：首次计数前不画、计数中再要求会再数一次、旧计数的图要等、同样的图不重画）。
- `catmail-gui`：`tray/mod.rs` 的 `Tray` 只剩托盘项和模型；`linux.rs`、`macos.rs` 去掉各自的 `icon()` 和
  `include_str!`。
- 文档：background.md 的托盘一节、gui.md 的结构和图标一节。
