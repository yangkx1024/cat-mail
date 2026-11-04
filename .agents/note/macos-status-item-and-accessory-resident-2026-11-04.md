# macOS 状态栏图标与常驻时隐藏 Dock 图标

- 日期：2026-11-04
- 类型：功能

## 背景

#106 的第五步在 macOS 上的部分（#108）：关掉窗口后要能从状态栏看到有没有新邮件、回到窗口、写邮件、
同步、退出。此前 macOS 关掉窗口后什么也不做（保持 Regular、Dock 图标），没有状态栏图标，也没有任何
地方显示未读数；设计文档把"只留状态栏图标"的模式留给这个 issue 决定。

## 决策

- 常驻时切到 Accessory（GPUI Fast 在 macOS 上的 `WindowingRequest::Headless` 就是它）：没有 Dock
  图标、菜单栏，不在 Cmd-Tab 里，只留状态栏图标。与 Linux、Windows 是同一个状态，"关闭窗口后保持运行"
  三个平台意思相同；去掉了 `GOES_HEADLESS` 和 `AfterLastWindow::Stay`。考虑过保留 Dock 图标（Mail.app
  的习惯），由用户选了隐藏；从启动台、Finder 再打开仍经 `on_reopen` 回到窗口。
- 回到窗口时 macOS 上先 `cx.activate(true)`：状态栏菜单不让应用成为前台，刚从 Accessory 切回的应用也
  不是，新窗口会开在别的应用后面。
- 图标是新画的单色模板图（`catmail-menu-bar.svg`，信封和猫爪），系统按菜单栏着色，不用跟外观重画；
  未读数写在图标右边（按钮标题），不画红色角标：菜单栏里彩色图标显得突兀。同一个数也作 Dock 角标。
- 直接用 `objc2-app-kit`，不用 `tray-icon`：GPUI 的 macOS 平台已经依赖 objc2 0.6 / objc2-app-kit 0.3，
  Cargo.lock 不多一个包；`tray-icon` 在 macOS 上做的也是这些调用，还带来 `muda` 和它的事件约定。
- 平台接口改成：图标的样子由 `tray/mod.rs` 的 `icon` 按平台定（带角标的应用图标在 `tray/badged.rs`，
  只在用它的平台上编译；macOS 是模板图），`Item::show` 收画好的位图（`None`：与上次一样）和未读数，
  提示、标题、角标由平台自己从未读数得出。共用部分仍在后台画、一次一个（`Coalesce`），测试里的托盘项
  照旧走这条路。`Tray` 记着上次画的 SVG，同样的不再画：起初 macOS 每次数变、外观变都重画同一张模板图
  （简化审查指出）；放在共用部分而不是 macOS 里特判，以后的平台也一样。
- 点击状态栏图标打开菜单（macOS 的习惯），"Open catmail" 在菜单第一项；Linux 仍是点击打开窗口。

## 方案

- `catmail-gui/src/tray/macos.rs`：`NSStatusItem` + `NSMenu`，菜单项的 target 是 `define_class!` 的
  小类（tag 为在 `MENU` 里的位置，经 channel 交给 `Tray::choose`）；BGRA 转预乘 RGBA 放进
  `NSBitmapImageRep`，组成 18 pt 的模板 `NSImage`；`NSDockTile::setBadgeLabel`。
- `tray/mod.rs`：按平台选模块和图标，没有托盘的平台只在模块顶上放行一次 `dead_code`，`pixmaps` 改成画
  任意 SVG；`tray/badged.rs`；`tray/linux.rs` 跟着接口改。`resident.rs`：macOS 也进 headless，显示窗口前激活。
- 依赖：`objc2`、`objc2-app-kit`、`objc2-foundation`（只在 macOS，版本与 GPUI 相同）。
- 文档：`docs/architecture/background.md`（两个状态、单实例、托盘），图标 README。
- 验证：新单元测试 `the_menu_bar_template_is_drawn`（两个尺寸都画出、不透明的比例合理）、
  `an_icon_that_looks_the_same_isnt_drawn_again`（去掉跳过时失败）；模板图放大
  导出 PNG 看过。Linux 上没有 macOS 的 SDK，GPUI 的 macOS 构建脚本跑不了：用官方 1.98.1 工具链加
  aarch64-apple-darwin 的 std，把 `macos.rs` 放进一个桩出父模块的临时 crate，`cargo clippy -D warnings`
  通过；整个 crate 在 macOS 上的 clippy 和测试靠 CI。没有在真的 Mac 上运行过，要手动看：状态栏图标与
  数字、菜单四项、关掉窗口后 Dock 图标消失、从菜单和启动台回到窗口且窗口在前面、Dock 角标、Accessory
  切回 Regular 后菜单栏能点。
