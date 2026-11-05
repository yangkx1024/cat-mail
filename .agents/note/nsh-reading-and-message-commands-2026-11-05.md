# nsh 版的读信区与邮件命令

- 日期：2026-11-05
- 类型：功能

## 背景

nsh 分支 ROADMAP 的 P4：读信区要有 gpui 版的工具栏和命令，列表悬停的行和勾选的邮件用同一套按钮，
远程图片、附件、在浏览器打开也要对齐。

## 决策

- 命令的规则用 `catmail_frontend::commands`、`list::Removal`、`reader::Header`（见同日的抽取记录），nsh 版只写界面。
- 按钮是自画的图标按钮（`commands::icon_button`）：悬停高亮用鼠标进出的信号，不用 `hover` 样式，因为它只取
  构建时的颜色，跟不上外观切换。按钮按组生成（`commands::group`），读信区在两组之间插 Labels，与 gpui 版顺序相同。
- 通知：nsh-base 只有无样式的 toast 原件（`ToastManager`、`toast_stack`），样式和计时写在 `toast.rs`；只在有
  通知时跑 50 ms 的计时任务，常驻时不空转。Escape 留给关读信区（`close_on_escape(false)`）。`report` 的错误也改成
  通知，侧栏底部不再显示最后一条错误。
- 远程图片不重新加载页面：注入脚本收到 `post_message` 的 `i:<url>\n<data:>` 后替换对应的 `img` 和
  `background`；脚本每次加载报 `r`，读信区再把已显示的图片发一遍（外观变化会重建页面）。拉取任务记在
  读信区，换邮件时取消，引擎里的下载随之中止。
- 打开即已读：`list::open` 是用户打开一行（和移走后列表打开下一封）的唯一入口，在那里标记；刷新后恢复选中不经过它。
  设置在启动时读一次（`shell::settings`），写入随 P7。
- Reply / Forward 等 P5 的写信窗口；正文查找要自己做（WebKit 没有查找栏，nsh 没暴露 `WebKitFindController`），
  先不做；内嵌图片仍等取完再建页面。

## 方案

- 新文件：`crates/catmail-nsh/src/commands.rs`（运行命令、等移走、永久删除的 `alert_dialog`、"Move to"/"Labels"
  选择框、被拒改动）、`reader.rs`（从 `app.rs` 移出并扩展：工具栏、头部与 Details、附件保存、远程图片、
  在浏览器打开、打不开时 Try again）、`toast.rs`。
- `list.rs`：`Removal` 接进读取与重选；悬停行的按钮、勾选时的工具栏；快捷键 E、Delete、Backspace、L、
  Ctrl+Shift+M/U/L。`page.rs`：桥接脚本收图片、报加载完成。`icons.rs`：新图标。
- `flake.nix`：开发 shell 的 `XDG_DATA_DIRS` 加上 gtk4 和 gsettings-desktop-schemas 的 GSettings schema。
  GTK 的保存对话框（`gtk::FileDialog`）要读 `org.gtk.gtk4.Settings.FileChooser`，找不到时 GLib 直接 abort
  （`cargo run` 点附件就崩）；打包时由 `wrapGAppsHook` 做同样的事。
- `shell.rs`：窗口上挂一个捕获阶段的 `GestureClick`，按在有焦点的输入框以外就清掉焦点（`set_focus(None)`），
  否则点了行之后单字母快捷键仍被搜索框吃掉。nsh 的后端只把按下交给最里层处理它的视图，应用里没有哪个视图
  能看到所有按下，只能在窗口这一层做；手势不认领事件，点击照常传下去。
- 验证：clippy、测试通过；截图看了浅色、深色下打开邮件时的工具栏和头部。点击、快捷键、对话框需要手动试。
