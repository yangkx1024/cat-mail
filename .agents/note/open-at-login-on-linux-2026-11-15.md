# Linux 开机启动

- 日期：2026-11-15
- 类型：功能

## 背景

macOS 的 "Open at login" 已经做了（`open-at-login-on-macos-2026-11-11.md`），Linux 一直是"不可用"，Settings 里
不显示开关。nsh 刚合入了统一的开机自启 API（longbridge/nsh#1884），先看能不能直接用。

## 决策

- **不用 nsh 的 API**：它只给 `run_app` 起的独立应用视图配后端，我们自带外壳，视图是嵌入的，拿到的永远是
  不可用；后端都是 `pub(crate)`。而且 Flatpak（我们 Linux 的发行形式）里它直接返回不可用，XDG 文件的 `Exec`
  也不能带 `--background`，macOS 上没有"是不是登录时的启动"，等批准读作 false。也不在 0.5.1 里。
- **包外**：自己写 `$XDG_CONFIG_HOME/autostart/net.yangkx.catmail.desktop`，`Exec` 带 `--background`，文件就是
  设置（桌面关掉的 `Hidden=true`、`X-GNOME-Autostart-enabled=false` 读作关）。按 app id 命名，不区分哪份拷贝写的，
  和 macOS 按 bundle id 一致；不检查 `Exec` 是不是自己（nsh 那样做），换一个路径打开时直接重写。
- **Flatpak**：Background portal 的 `RequestBackground`（`autostart`、`commandline` = `catmail-gui --background`）。
  portal 没有读的接口，沙箱也看不到宿主的 autostart 文件，只能记住 portal 上次的答复（配置目录里的空文件
  `open-at-login`）。配置目录由外壳在启动时告诉 `autostart`（`use_config_dir`），与设置文件同一处，不让
  `autostart` 自己再找一遍（否则 `CATMAIL_CONFIG_DIR` 对它无效，两处各自决定前端状态放哪）。放弃了 `--filesystem=xdg-config/autostart:ro`：为显示一个开关多要文件权限不划算。也没放进
  `settings.json`：记录和 portal 调用都在 `autostart` 的那把锁里，一起串行。
- 直接用树里已有的 zbus（blocking API），没有引入 ashpd。

## 方案

- `catmail-frontend/src/autostart/linux.rs`：XDG 文件的读写，`Exec` 按规范引号转义；Flatpak 时交给
  `autostart/portal.rs`。portal 的请求对象按调用者唯一名和 `handle_token` 算出，先订阅 `Response` 再调用。
- `APP_ID` 挪到 `catmail-frontend`（gui 的 `shell` 重新导出），`in_flatpak` 从托盘挪到 crate 根共用。
- 新的 workspace 依赖 `zbus`（版本和特性与树里已有的一致）。
- 测试：XDG 条目的引号转义、开关与桌面设置的交互；portal 在测试自己起的 `dbus-daemon` 上对着假 portal（在调用
  返回前就发出答复）测开、关、不允许后台、关掉对话框、portal 不答复就离开总线、答复记不下来。后两种是 Codex 的 review 指出的：
  zbus 的信号流在名字换主人时不会结束，原来会一直等下去、占着锁；记录写不了时原来报成功、显示旧的状态。等
  答复改用 zbus 的异步接口（`pollster` 驱动），同时等接下请求的 portal（回复的发送方，唯一名）离开总线。
  一开始看的是 well-known 名，Codex 再次 review 指出：portal 由总线按需启动时名字也换主人，会被当成离开。
  测试用自己配置的 dbus-daemon（只认测试的服务目录），按需启动本测试二进制里被忽略的测试当 portal：晚一点
  答复的、答复前退出的各一个。生成的条目过了 `desktop-file-validate`。
- 真 portal：从 Flatpak 沙箱里按同样的参数调用 xdg-desktop-portal，开时答复
  `(0, {background: true, autostart: true})`，portal 写出
  `Exec=flatpak run --command=catmail-gui net.yangkx.catmail --background`；关时 `autostart: false`，文件删掉。
  文件由 xdg-desktop-portal 自己写，只要求桌面的 portal 配置把 Background 交给某个后端，否则接口不存在。
- 没验证：真实的登录启动。
- 文档：background.md「开机启动」、packaging.md、ROADMAP P9。
