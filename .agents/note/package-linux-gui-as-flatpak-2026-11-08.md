# Linux 图形界面打包为 Flatpak

- 日期：2026-11-08
- 类型：功能

## 背景

换成 nsh 之后 GUI 链接 GTK 4 和 WebKitGTK 6.0，GPUI 时代的 AppImage 撤下了，Linux 只发 CLI。需要重新选一个
能带上 WebKitGTK 的格式。

## 决策

- **Flatpak（GNOME runtime 51），不用 AppImage。** 查了 WebKit 源码：release 构建的辅助进程路径只认编译时的
  `PKGLIBEXECDIR`（`WEBKIT_EXEC_PATH` 只在开发者构建里生效）；6.0 的网页进程一定跑在 bwrap 里，沙箱只挂系统目录和
  `LD_LIBRARY_PATH`，看不到 AppImage 的挂载点。AppImage 只能改 `.so` 的字符串并关沙箱，还要随每个 WebKit 安全
  公告重发。GNOME runtime 自带 WebKitGTK 6.0、GTK、GStreamer 并由 Flathub 更新，WebKit 的子沙箱在 Flatpak 里
  正常工作。发行版的包（.deb 等）可以以后补，不替代 Flatpak。
- **离线依赖用 `cargo vendor`**，不用 flatpak-cargo-generator：本地和 CI 发 bundle 足够，不必引入外部脚本。
  上 Flathub 时再换。
- **托盘在 Flatpak 里不占 `org.kde.StatusNotifierItem-<pid>-<id>`**：沙箱里 pid 都是同一个小数字，会撞名，
  而且 `--own-name` 不接受这种通配（flatpak-builder 报 `Invalid dbus name`）。ksni 的 `disable_dbus_name` 用
  唯一名注册，按 `/.flatpak-info` 判断。
- **通知点击：应用自己占用 app id 的总线名。** Flatpak 里 GLib 用 Notification portal，GLib 的 portal 后端不听
  `ActionInvoked`，点击由 xdg-desktop-portal-gtk 对 `net.yangkx.catmail` 调用
  `org.freedesktop.Application.ActivateAction`；应用是 `NON_UNIQUE`，不占这个名字，点击就没反应（review 发现，
  实测 `ServiceUnknown`）。GApplication 在 `NON_UNIQUE` 时照样导出接口和 action，所以在 startup 时
  `bus_own_name_on_connection`（`DO_NOT_QUEUE`）就够了，不改成 D-Bus 单实例（单实例按数据目录，靠 socket）。
  原生 GNOME Shell 上点击无效的已知限制也一并解决。
- 密钥环直接 talk `org.freedesktop.secrets`（`keyring` 走 zbus 的 Secret Service），没有改成 Secret portal。
- CLI 一起打进包：沙箱的数据目录在 `~/.var/app/` 下，包外的 CLI 看不到。
- CI 用 `ubuntu-24.04(-arm)`，apt 装 flatpak-builder，先关掉 AppArmor 对非特权 userns 的限制。

## 方案

- `packaging/linux/net.yangkx.catmail.yml`：manifest，权限只有网络、显示、`/dev/dri`、secrets 和
  StatusNotifierWatcher。
- `packaging/linux/build-flatpak.sh`：vendor、构建、可选安装、`flatpak build-bundle`，输出到 `target/flatpak/`。
- `crates/catmail-frontend/src/tray/linux.rs`：Flatpak 里 `disable_dbus_name(true)`。
- `crates/catmail-gui/src/shell/gtk.rs`：`own_app_id`。测试在 `dbus-run-session` 起的私有总线上（子进程重跑
  本测试二进制，GLib 每个进程只读一次总线地址），从另一条连接按名字调用 `ActivateAction`，不占名字时失败。
- `.github/workflows/release.yml`：新 `flatpak` job，bundle 随 release 发布。
- 文档：`packaging.md` 的 Linux 图形界面一节、`background.md` 开机启动（Flatpak 要用 Background portal）、README。
- 验证（本机 NixOS，headless weston，Dovecot 测试环境）：bundle 17 MB；沙箱里 `catmail account add` 写入
  gnome-keyring，`sync` 读回密码；GUI 打开 HTML 邮件，WebKitWebProcess 跑在 `flatpak-spawn --sandbox` 的子沙箱里
  并渲染出正文（截图）；托盘以 `:1.x/StatusNotifierItem` 注册，提示 "5 unread messages"；投递新邮件后 GUI 调用
  Notification portal 的 `AddNotification`；按 portal 的方式从宿主机调用 `ActivateAction`（`new-mail/2/14`）后
  窗口转到那封邮件（它被标为已读）；第二次启动交给已运行的实例并退出。没验证：附件打开和保存、链接
  （portal，要手动点）、aarch64、CI 上的构建。
