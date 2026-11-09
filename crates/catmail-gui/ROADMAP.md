# catmail-gui roadmap

`catmail-gui` 基于 Native Shell（nsh）：Linux 上 GTK 4 和 WebKitGTK，macOS 上 AppKit 和 WKWebView。它起初是
`nsh-spike` 分支上的试验前端 `catmail-nsh`，逐项对齐到原来基于 GPUI 的 `catmail-gui`；对齐之后 GPUI 版删掉了，
由它接替 `catmail-gui` 的名字（为什么换见 [docs/architecture/gui.md](../../docs/architecture/gui.md#选型)）。

设计以 gui.md 为准，这里记进度、还没做的事，以及 nsh 没有对应能力、要换做法的地方（标 ⚠️）。

**做法**

- **一项一提交**，在 `.agents/note/` 留记录。
- **纯逻辑放进 `catmail-frontend`，不放这里**：不依赖窗口的模型（文件夹分区、事件刷新的节流、日期格式、HTML 策略、
  设置文件、主题推导与导入……）在 frontend，单测覆盖；这个 crate 只有视图和外壳。

## P0 只读三栏

- [x] 文件夹、邮件列表（虚拟列表、滚到底取下一页）、读信区
- [x] 正文在 web view 里：CSP 断网，图片由读信区经自定义 scheme 应答，页面 JS 关闭，注入脚本回报高度和链接
- [x] 暗色重新着色（`serve_images` 建页时同一遍里改色）
- [x] 三栏可以横向拖动（侧栏 192–384 px，读信时列表 256–640 px）。⚠️ 宽度由应用自己记，用 nsh-base 单独的
      `resize_handle`：它的 `h_resizable` 快速来回拖时会丢掉拖动结束，之后分隔线错位；每次拖动从开始时的宽度
      算起，丢了结束也不会累积

## P1 应用骨架

- [x] 自建 `gtk::Application` / `NSApplication`，每个窗口放一个 `AppView`，替换 `run_app`（`shell/`）。引擎由应用
      持有（`engine.rs`），主循环结束后 `shut_down` 并等它关好；Ctrl+Q、Ctrl+W、SIGINT / SIGTERM 都走这个退出流程
- [x] 引擎异步打开（`open_async`）：先显示窗口，打不开时显示原因
- [x] 单实例（`catmail_frontend::instance`）：再次启动时唤起已有窗口；Wayland 下用启动方的 activation token 获得焦点
- [x] 日志写进数据目录（`logging::write_file`）
- [x] 读 `settings.json`，应用外观和文字大小（`AppView::set_font_scale`）
- [x] 图标：Heroicons SVG，`include_str!` 后用 nsh 的 `svg` 画，`currentColor` 跟随文字色（`icons.rs`）

## P2 侧栏（gui.md「侧栏」）

- [x] All Inboxes、有角色的文件夹、Gmail 的 Labels 一节、本机草稿、有待发时才出现的 Outbox、连接问题的说明
- [x] 侧栏一次读完（`overview`）；`DraftsChanged` 只重数该账户的草稿。引擎事件按 `Refresh::after` 决定重读什么，
      200 ms 节流，读侧栏一次一个
- [x] 右键菜单新建、重命名、删除文件夹和标签，F2 重命名。菜单是原生的 `popup_menu`，删除确认是原生的
      `alert_dialog`，命名用 `dialog()` 加输入框
      ⚠️ Linux 上只有一项的菜单下方比上方空：菜单项所在的滚动窗口不矮于竖向滚动条（GTK 主题给滑块 40 px），
      shell 自己加一条样式把滑块缩短（`shell/gtk.rs` 的 `MENU_CSS`），nsh 有了这条（longbridge/nsh#1359）就删
- [x] 打开文件夹时同步它（`sync_folder`）
- [x] 账户折叠（一个账户默认展开，多个默认折叠）、子文件夹折叠
- [ ] 折叠的动画
- [x] Compose（Ctrl+N）、Settings 按钮、Sync 按钮（在搜索框左边，同步时转圈，nsh 的循环动画）

## P3 邮件列表（gui.md「邮件列表」）

- [x] 按天分组。⚠️ nsh 列表的 sticky header 只能在建表时给一次，分组随数据变，所以标题是列表里的普通行，不吸顶
- [x] 时间按系统的 12 / 24 小时设置（nsh 的 `locale_settings()`），列表、读信区头部、发件箱都跟着设置变
- [x] 两种行形：读信区关着时一行排开，打开后两行；切换时把选中的那封滚到中间。nsh 列表按行量高度，两行时主题
      按实际行数占高
- [x] 头像、未读只用字重、Gmail 用户标签；悬停的行显示 To / Cc 和操作按钮
- [x] 勾选多封（点头像或 X）、按标记勾选的菜单（原生 `popup_menu`）、勾选时的批量工具栏
- [x] 搜索：200 ms 防抖；先显示本地结果，1 s 后再问服务器
- [ ] 搜索框获得焦点时提前登录（`connect_for_search`，见 docs/architecture/storage.md「问服务器」）
- [x] 刷新：节流、合并请求、结果没变不替换、按行的身份保持选中；选中的邮件消失时清空读信区
- [x] 草稿、发件箱两个来源；Escape 关上读信区

## P4 读信与操作（gui.md「读与改」「读信」）

- [x] 工具栏、勾选工具栏、悬停的行用同一套按钮（`commands::group`）；快捷键 E、Delete / Backspace、L、
      Ctrl+Shift+M / U / L，作用于勾选的邮件，没有勾选时是选中的那封
- [x] Reply、Reply all、Forward（Ctrl+R、Ctrl+Shift+R、Ctrl+Shift+F）
- [x] 打开即已读；归档、删除、移动、去掉标签后打开下一封（`Removal`）；永久删除先确认（原生 `alert_dialog`）
- [x] "Move to"、"Labels"对话框
- [x] 服务器拒绝的改动用通知说明。⚠️ nsh-base 只有无样式的 toast 原件，通知的样式和计时在 `toast.rs`；出错也
      改成通知，不再写在侧栏底部
- [x] 远程图片：先显示缓存里的；点击后逐批显示，换邮件即取消。不重新载入页面：页面早就请求了这些图片，
      读信区把请求留着，图片到了再应答
- [x] 打开时先显示文字、内嵌图片后到，同样是留着的请求取到一张应答一张
- [x] 图片由 WebKit 自己读：nsh 的 URL scheme handler（longbridge/nsh#1136，#1233）。页面里的 `cid:` 和远程
      图片改写成 `catmail-image:` 的 URL，CSP 的 `img-src` 放行它；不再把 `data:` URL 写进脚本一次发一张。
      ⚠️ Linux 上限流：WebKitGTK 按 8 KB 一块读 scheme 的应答，大图一起应答时下一封邮件要等（见 gui.md 的图片）；
      同时在传的不超过 1 MiB，页面报一张到了再放行（`html::pacer`）。WebKitGTK 按大块读之后去掉
      （WebKit bug 330624）；不放进 nsh，原因见 gui.md 的图片。macOS 不限流，还要用
      图片多的邮件看一眼
- [x] 附件（系统保存对话框，写文件在另一个线程）、头部（"Details"）、在浏览器打开（`hand_off`）
- [ ] 正文的查找：WebKit 没有自带的查找栏，nsh 也没给 `WebKitFindController`，要自己用注入脚本做。选中、复制、
      全选是 WebKit 自带的

## P5 写信窗口（gui.md「写信窗口」）

每个视图跑在自己的 nsh 线程上，写信窗口的视图和主线程之间用 `shell::on_main` 与一个关窗请求的通道来回。

- [x] 每封草稿一个窗口；一开始就是草稿，没改就关窗时删掉；停顿 800 ms 自动保存，同一草稿的引擎调用不重叠
- [x] 地址栏的解析和显示、From 下拉（原生 `popup_menu`）、附件（选文件或拖进窗口，nsh file-drop）
- [x] Ctrl+Enter 发送；关窗按钮、Ctrl+W、Escape 走同样的流程：GTK 的 `close-request` 总是拦下，交给视图决定，视图
      存好后自己销毁窗口
- [x] 退出前先关所有写信窗口，有一个关不了就不退出
- [x] 草稿的右栏：Edit、Delete draft；双击或 Enter 打开

## P6 发件箱（gui.md「发件箱」）

- [x] 右栏按条目状态给出操作，可能重复的 Send again 先用 `alert_dialog` 确认
- [x] 后台发送；`unknown` 的条目从不自动重发（引擎的规则）

## P7 设置与主题（gui.md「主题与设置」）

- [x] 设置对话框：General、Appearance、Accounts、Reading；Ctrl+,，Ctrl + / Ctrl − / Ctrl 0 改文字大小。改了就写回
      `settings.json`，所有窗口（包括写信窗口）随之更新
- [x] 主题：gpui-kit 主题 JSON 的颜色键映射到 shadcn 的 token，再加上应用自己的，经 `set_theme_overrides` 给每个视图；
      导入各种终端配色格式
- [x] 跟随系统外观（nsh 自带，经 XDG portal）

## P8 登录页（gui.md「登录页」）

- [x] 没有账户时自动打开；`add_account(.., Verify::Login)`；进行中不能取消
- [x] 表单字段与发件身份的表单共用；任一字段按 Enter 提交

## P9 常驻与系统集成（docs/architecture/background.md）

- [x] 托盘：Linux 是 ksni（frontend 的 `tray::linux`），菜单 Open catmail、New message、Sync now、Quit。图标用 resvg
      在另一个线程上画（nsh-core 已经带了 resvg，这里另外打开 `text` 画角标数字）。亮暗跟随 portal 的
      `color-scheme`，即桌面的外观而不是应用的，常驻时也跟随
- [x] 关窗后常驻，重开时回到上次的位置（`shell::left_at`）。Quit（Ctrl+Q、托盘、SIGHUP / SIGINT / SIGTERM）先关写信
      窗口，第二个信号直接退出；日志记 `quitting reason=…`。⚠️ GTK 常驻时仍连着显示服务器
- [x] 新邮件通知（Linux）：⚠️ nsh 没有通知，改用 GLib 的 `GNotification`。点击走应用的 `open-mail` action，打开
      对应账户的 Inbox，列表读到那封后选中它（`Reveal`）。GNOME Shell 上点击无效：GLib 在那里改用 GNOME 自己的
      接口，它经 D-Bus 名字唤起应用，而应用不占这个名字（`NON_UNIQUE`，单实例靠 socket）
- [x] 系统唤醒：nsh 不听 logind，靠引擎自己发现睡眠（`sleep.rs`，最多晚 30 秒）
- [ ] 系统唤醒后马上同步：自己听 logind 的 `PrepareForSleep(false)`（Linux，zbus 已随 ksni、keyring 进来）和
      `NSWorkspaceDidWakeNotification`（macOS），调 `MailClient::resume()`。GPUI 版在 GPUI 的 `on_system_wake` 里调用，
      醒来就同步；现在要等引擎自己发现
- [ ] macOS 常驻时切到 Accessory 激活策略：只留状态栏图标，Dock 图标和应用切换器里都没有，开窗时切回 Regular
      （GPUI 版就是这样）。现在关窗后 Dock 图标一直在
- [ ] 退出时等引擎关闭要有上限：现在 `shell::quit` 之后 `pollster::block_on(client.closed())` 不限时，引擎里一次卡住的
      写入就让进程退不出去（会话结束时系统等不到它退出）。GPUI 版最多等 GPUI 给的 `SHUTDOWN_TIMEOUT`；到时间就记一条
      日志直接退出
- [ ] 确认 Linux 新邮件通知的正文没有转义两次：frontend 的 `new_mail::plain` 按 XDG `body-markup` 替通知服务器转义了
      `&`、`<`（GPUI 版直接给 `org.freedesktop.Notifications`），现在经 `GNotification`，要看 GLib 的 freedesktop
      后端会不会再转义一次。用主题带 `&` 的邮件在 GNOME 和 KDE 上各试一次；重复了就让 Linux 也给纯文本
- [x] 常驻时回收内存：关掉最后一个窗口 2 秒后 `malloc_trim`。实测（release，两个真实账户，2026-11-05），单位 MB，
      RSS / PSS：

      | | 主进程 | WebKit 渲染进程 | WebKit 网络进程 | 合计 PSS |
      |---|---|---|---|---|
      | 刚启动、还没开邮件 | 226 / 193 | — | — | 193 |
      | 开着窗口读信 | 280 / 192 | 195 / 107 | 69 / 32 | 约 335 |
      | 关窗后、trim 后 | 265 / 197 | 退出了 | 69 / 41 | 约 238 |

      trim 只还回约 14 MB（主进程 anon 从 104 降到 90；GPUI 版当时能还回约 76 MB，常驻约 151 MB RSS）。常驻时主进程
      里约 107 MB 是文件映射（程序本身和 GTK、WebKitGTK 的库），能和别的进程共用，也能被换出。渲染进程随窗口退出，
      网络进程留着：bwrap 沙箱和 `xdg-dbus-proxy` 合计约 3 MB
- [x] 读信区和它的 web view 随主窗口一直存在，渲染进程开窗时就起好、换邮件不换（原来每封一个，约 100 MB PSS，
      还要起一次 bwrap 沙箱）。⚠️ 没选中邮件时藏在列表下面，不用 `invisible`：一阵没画的 web view 再画时，WebKit
      画好之前会先闪一帧黑。开窗后点开第一封，内容约 2 帧（约 33 ms）就出现，原来约 13 帧
- [ ] 常驻时网络进程还留着（41 MB PSS），可它从来不联网（CSP 只放行 `data:` 和读信区自己的 scheme）。原因在 nsh：它按 app id 把
      `NetworkSession` 一直存在表里，最后一个 web view 没了也不放。上游已改成弱引用（longbridge/nsh#1051），nsh
      的 rev 已升级。还要在 Linux 上用真实账户确认关窗后网络进程退出，测一下重新开窗要多花多少时间，再更新
      background.md 里的数字
- [x] 快捷键：Sync（Ctrl+Shift+N、F5）、Ctrl+F 聚焦搜索框、写信窗口的 Escape 关闭和 Ctrl+Shift+A 加附件

## P10 平台与工程

- [ ] 字体：Pango 自己做回退，要验证中文各字重、粗体是否正常
- [ ] 测试：用 nsh-testing 无头驱动窗口（读信、写信、设置、文件夹）
- [x] 在主 workspace 里：longbridge/nsh#966 在上游 main 修好（#1022），还没发版，先用 git 依赖钉在 main 的 commit 上
      （根 `Cargo.toml`）。nsh 的 Linux 后端从此总是链接 GStreamer（声音），开发 shell 和 CI 都装了它的开发包
- [ ] nsh 发版后换回 crates.io，并把仓库移出 `deny.toml` 的 `allow-git`
- [x] cargo deny：许可证（webpki-roots 的 CDLA-Permissive-2.0，target-lexicon、winx 的 LLVM-exception）在
      `deny.toml` 里逐个放行；nsh-updater 换成 zip 9（longbridge/nsh#1164）后，zip 8 的四个 advisory 不再忽略
- [x] GTK 4.22 上 `gtk_widget_measure` 警告刷屏：`NshView::size_allocate` 不 measure 就 allocate 子 widget，上游
      longbridge/nsh#1048 已由 longbridge/nsh#1049 修好，nsh 的 rev 已升级（还要在 GTK 4.22 上看一眼警告没了）
- [ ] 内存、滚动帧率、冷启动时间的基准
- [ ] **Linux 的安装包**：AppImage 已从发布里撤下，Linux 上现在只发 CLI。要重新做（AppImage 或别的格式，如 Flatpak），
      主要是带上 WebKitGTK：它的辅助进程（WebKitWebProcess、WebKitNetworkProcess）的路径是编译进库里的，还用 bwrap
      沙箱。运行时依赖 GTK 4.14+ 和 WebKitGTK 6.0，Ubuntu 22.04 没有 WebKitGTK 6.0，glibc 2.35 的基线要重新定。
      `.desktop` 和 AppStream 文件留在 `packaging/linux/`
- [x] macOS 后端（AppKit，正文在 WKWebView）：`shell/` 分成共用的部分和两个工具包（`gtk.rs`、`appkit.rs`），窗口、
      退出流程、常驻、设置和主题两边共用。⚠️ AppKit 没有 GLib 那样的主循环 future 执行器，用 `async-task` 在主
      dispatch queue 上跑；输入框失焦自己用事件 monitor 做（同 Linux）。菜单栏、停主循环、外观映射抄了 nsh `run_app`
      的私有实现，失焦两个后端各一份，宜由 nsh 提供（#248）。菜单栏有应用菜单（⌘Q 走退出流程）、Edit、Window（⌘W
      走窗口的关闭询问），点 Dock 图标唤回主窗口；信号用 `ctrlc`。快捷键提示用 nsh 的 `key_label`
- [x] macOS 状态栏图标（frontend 的 `tray::macos`）：计数、绘制与 Linux 共用（`tray/mod.rs`），模板图用 resvg 画一次，
      未读数写在图标旁和 Dock 角标上
- [x] macOS 窗口外框（`chrome.rs`，nsh 的 `TitleBarStyle::Hidden` 和 `window_drag`）：主窗口一体式，红绿灯在侧栏
      左上角、各栏顶部那一行里（52 pt，AppKit unified 工具栏给按钮的那一行高；nsh 居中红绿灯，高度从
      `title_bar()` 读，longbridge/nsh#1068 已修），这一行按下拖动窗口。写信窗口保留标题栏，但是应用自己
      画的
- [x] macOS 磁盘映像打包这个 `catmail-gui`（`packaging/macos/build-dmg.sh`）：菜单栏显示应用名、Dock 有图标。
      `cargo run` 起的没有 bundle，仍显示可执行文件名
- [ ] macOS：新邮件通知（`UNUserNotificationCenter`，要在 bundle 里运行）；注销时不取消（现在
      `applicationShouldTerminate` 先取消，等写信窗口存完再退出）
- [ ] **Windows**：接上 nsh 的 Windows 后端（`nsh-platform-windows`：Win32、Direct2D、DirectWrite、正文在 WebView2）。
      要做的是外壳的第三个工具包（窗口与主循环、信号与退出）、托盘（frontend 的 `tray` 里加 Windows 的实现，#109）、
      新邮件通知；单实例已在 `catmail_frontend::instance`，三个平台一套。再加 Windows 的打包。之前 Windows 上只有 CLI
