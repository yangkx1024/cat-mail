# catmail-gui roadmap

`catmail-gui` 基于 Native Shell（nsh）：Linux 上 GTK 4 和 WebKitGTK，macOS 上 AppKit 和 WKWebView。它起初是
`nsh-spike` 分支上的试验前端 `catmail-nsh`，逐项对齐到原来基于 GPUI 的 `catmail-gui`；对齐之后 GPUI 版删掉了，
由它接替 `catmail-gui` 的名字（为什么换见 [docs/architecture/gui.md](../../docs/architecture/gui.md#选型)）。

设计以 [gui.md](../../docs/architecture/gui.md) 为准，这里记进度、还没做的事，以及 nsh 没有对应能力、要换做法的地方（标 ⚠️）。

**当前剩余未完成项**（完整上下文见下文各节）：
- P4：正文的页内查找（需注入脚本实现）
- P5：正文里直接拖入图片作为内嵌图
- P10：内存、滚动帧率与冷启动时间基准；Flatpak 上架 Flathub；Windows 后端与打包

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
- [x] 打开文件夹时同步它（`sync_folder`）
- [x] 账户折叠（一个账户默认展开，多个默认折叠）、子文件夹折叠
- [x] 折叠的动画：`Collapsible` 的 `reveal` 弹簧收拢高度，箭头转动
- [x] Compose（Ctrl+N）、Settings 按钮、Sync 按钮（在搜索框左边，同步时转圈，nsh 的循环动画）

## P3 邮件列表（gui.md「邮件列表」）

- [x] 按天分组。⚠️ nsh 列表的 sticky header 只能在建表时给一次，分组随数据变，所以标题是列表里的普通行，不吸顶
- [x] 时间按系统的时区和 12 / 24 小时设置（nsh 的 `locale_settings()`），列表、读信区头部、发件箱都跟着设置变；
      换了时区或过了午夜，日期标题重新分组
- [x] 两种行形：读信区关着时一行排开，打开后两行；切换时把选中的那封滚到中间。nsh 列表按行量高度，两行时主题
      按实际行数占高
- [x] 头像、未读只用字重、Gmail 用户标签；悬停的行显示 To / Cc 和操作按钮
- [x] 勾选多封（点头像或 X）、视图过滤菜单（All / Read / Unread / Flagged / Unflagged，原生 `popup_menu`）、勾选时的批量工具栏
- [x] 搜索：200 ms 防抖；先显示本地结果，1 s 后再问服务器
- [x] 搜索框获得焦点时提前登录（`connect_for_search`，见 docs/architecture/storage.md「问服务器」）
- [x] 刷新：节流、合并请求、结果没变不替换、按行的身份保持选中；选中的邮件消失时清空读信区
- [x] 草稿、发件箱两个来源；Escape 关上读信区
- [x] 行的增删动画：重读前 `animate_layout`，留下的行滑动、新来的淡入；日期标题按日期作身份
- [x] 右栏打开、关上的动画：右栏固定在打开时的位置压在列表下，列表的边滑动露出、盖回
      ⚠️ macOS 上 nsh 的滚动指示器不跟着动画中的 frame 走（longbridge/nsh#3188），动画期间隐藏列表和侧栏的指示器（`motion::hide_indicators`）

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
- [x] 正文用 nsh 的 `RichEditor`：格式按钮、存 HTML 和纯文本、图片（选择、粘贴）作为内嵌附件
- [ ] 正文里拖进图片：现在拖进窗口的文件都当附件（窗口的 file-drop）
- [x] 链接：输入、粘贴的网址自动变链接（nsh 的 `link` 插件），底栏按钮和 Ctrl+K 的对话框插入、修改、去掉链接
- [x] 删掉正文里的图片后，它的内嵌附件在关窗时删掉（编辑中、发送失败后都留着，撤销还能带回来）
- [x] 回复、转发的引文在编辑器下面：收起的横栏，展开后和读信区一样显示（共用 `web/`）；去掉引文、在引文里回复

## P6 发件箱（gui.md「发件箱」）

- [x] 右栏按条目状态给出操作，可能重复的 Send again 先用 `alert_dialog` 确认
- [x] 后台发送；`unknown` 的条目从不自动重发（引擎的规则）

## P7 设置与主题（gui.md「主题与设置」）

- [x] 设置对话框：General、Appearance、Accounts、Reading；Ctrl+,，Ctrl + / Ctrl − / Ctrl 0 改文字大小。改了就写回
      `settings.json`，所有窗口（包括写信窗口）随之更新。下拉选择是原生的 `popup_menu`，当前项打勾
- [x] 主题：gpui-kit 主题 JSON 的颜色键映射到 shadcn 的 token，再加上应用自己的，经 `set_theme_overrides` 给每个视图；
      导入各种终端配色格式
- [x] 跟随系统外观（nsh 自带，经 XDG portal）
- [x] Agents：MCP 服务的开关、端口，令牌的增删（新令牌只在这里显示一次，可复制；加时选层级和账户）；服务由
      frontend 的 `mcp::Host` 在自己的线程上运行，引擎打开后启动，退出时先于引擎停下

## P8 登录页（gui.md「登录页」）

- [x] 没有账户时自动打开；`add_account(.., Verify::Login)`；进行中不能取消
- [x] 表单字段与发件身份的表单共用；任一字段按 Enter 提交

## P9 常驻与系统集成（docs/architecture/background.md）

- [x] 托盘：Linux 是 ksni（frontend 的 `tray::linux`），菜单 Open catmail、New message、Sync now、Quit。图标用 resvg
      在另一个线程上画（nsh-core 已经带了 resvg，这里另外打开 `text` 画角标数字）。亮暗跟随 portal 的
      `color-scheme`，即桌面的外观而不是应用的，常驻时也跟随
- [x] 关窗后常驻，重开时回到上次的位置（`shell::left_at`）。Quit（Ctrl+Q、托盘、SIGHUP / SIGINT / SIGTERM）先关写信
      窗口，第二个信号直接退出；日志记 `quitting reason=…`。⚠️ GTK 常驻时仍连着显示服务器
- [x] 新邮件通知（Linux）：⚠️ nsh 的通知要经过视图的根，常驻时没有视图，改用 GLib 的 `GNotification`。点击走应用的 `open-mail` action，打开
      对应账户的 Inbox，列表读到那封后选中它（`Reveal`）。为让 Flatpak Notification portal 和 GNOME Shell 的点击
      能经 `org.freedesktop.Application.ActivateAction` 唤起应用，`shell/gtk.rs` 在 startup 时主动占用 app id
      （`DO_NOT_QUEUE`，单实例仍由 socket 负责，见 `background.md`）
- [x] 系统唤醒后马上同步：⚠️ nsh 的 `wake_count()` 记在视图上，常驻时没有视图，所以自己听 logind 的
      `PrepareForSleep(false)`（Linux，GIO 的 D-Bus，Flatpak 里要 `--system-talk-name=org.freedesktop.login1`）和
      `NSWorkspaceDidWakeNotification`（macOS），调 `MailClient::resume()`（`wake.rs`）。听不到时靠引擎自己发现睡眠（`sleep.rs`，最多晚 30 秒）。⚠️ 还没在真机的
      睡眠上确认过（看日志的 `the frontend says the computer woke`）
- [x] 开机启动（macOS）：Settings › General 的 "Open at login" 是 `SMAppService.mainApp`（frontend 的 `autostart`），
      只在 bundle 里、macOS 13 起显示。`--background` 和登录时的启动不开窗口（`shell::launched`）；登录时的启动按
      启动的 Apple Event 判断，⚠️ 还没在真的登录上确认过（看日志的 `launched opened=…`）
- [x] 开机启动（Linux）：XDG autostart 文件，Flatpak 里经 Background portal 的 `RequestBackground`，都带
      `--background`（frontend 的 `autostart::platform`）。Flatpak 里的状态是 portal 上次的答复。Flatpak 里对真
      portal 的请求试过，写出、删掉都对。⚠️ 还没在真的登录上确认过；portal 配置里没有 Background 的桌面
      （wlroots 系常见）按开关会报错
- [x] macOS 常驻时切到 Accessory 激活策略：只留状态栏图标，Dock 图标和应用切换器里都没有，开窗时切回 Regular
      （`toolkit::resident`、`Window::present`）；`--background` 一开始就是 Accessory。⚠️ 有窗口（Regular）时在别的
      应用的全屏 Space 里点状态栏图标不出菜单，是 AppKit 的限制，见 background.md「两个状态」；要解决得把状态栏项
      拆成单独的 `LSUIElement` 辅助应用
- [x] 退出时等引擎关闭有上限：主循环结束后最多等 2 秒（`shell::ENGINE_CLOSING`，平时关引擎不到 1 ms），到时间记
      `the engine didn't close in time` 直接退出，引擎里一次卡住的写入不再让进程退不出去、会话结束时等不到它
- [x] Linux 新邮件通知的正文转义（主题带 `&`、`<`）：GLib 的 freedesktop 后端不转义，portal 和
      `org.gtk.Notifications` 不读标记，`new_mail/linux.rs` 照 GLib 的选路决定转不转义（#331，见
      background.md「Linux」）。Flatpak（`xdg-desktop-portal-gtk` + `noctalia`）已真机实测，`new_mail/linux.rs` 的
      `dbus-run-session` 测试覆盖了全部 5 条 GLib 选路（`body-markup`、无标记 freedesktop、`org.gtk.Notifications`、
      portal、`GNOTIFICATION_BACKEND=freedesktop`）
- [x] 常驻时回收内存：关掉最后一个窗口 2 秒后 `malloc_trim`。nsh 0.5.0+（longbridge/nsh#1051）与 web view 的
      `incognito` 之后，关窗时 WebKit 渲染进程和网络进程都在约 50–60 ms 内退出。2026-11-22 实测（Flatpak release，
      nsh 0.6.1，真实账户常驻关窗 + trim 后）：主进程 231 MB RSS / 216 MB PSS（anon 68 MB、file 148 MB），
      WebKit 渲染进程和网络进程均已退出，合计约 216 MB PSS（旧版网络进程滞留时约 238 MB，见 background.md）
- [x] 读信区和它的 web view 随主窗口一直存在，渲染进程开窗时就起好、换邮件不换（原来每封一个，约 100 MB PSS，
      还要起一次 bwrap 沙箱）。⚠️ 没选中邮件时藏在列表下面，不用 `invisible`：一阵没画的 web view 再画时，WebKit
      画好之前会先闪一帧黑。开窗后点开第一封，内容约 2 帧（约 33 ms）就出现，原来约 13 帧
- [x] 比读信区宽的邮件，横向滚动条在读信区底部，不用纵向滚到底才看得见（#355）。⚠️ nsh 的滚动区只沿一个方向
      滚（两个方向都滚要后端支持缩放，macOS、Linux 都不支持），所以页面藏起 body 自己的横向滚动条、回报横向
      滚动，读信区自己画一条（`web::Sideways`）
- [x] 常驻时网络进程随最后一个 web view 退出（longbridge/nsh#1051、nsh 0.6.1 + `incognito`）：Linux 上已用真实账户
      （Flatpak release）确认关窗后 `WebKitNetworkProcess` 和 `WebKitWebProcess` 均退出（约 50–60 ms）；重开窗口时
      重新拉起网络进程多花约 34 ms（WebKitGTK 2.54.1 / GTK 4.22.4 实测：冷启约 242 ms vs 网络进程常驻约 207 ms），
      数字已更新到 background.md
- [x] 快捷键：Sync（Ctrl+Shift+N、F5）、Ctrl+F 聚焦搜索框、写信窗口的 Escape 关闭和 Ctrl+Shift+A 加附件
- [x] 对话框开着时列表的键（E、X、L、Delete、Enter、Escape、F2 等）不作用于后面的邮件。⚠️ nsh 的键不按模态框
      分范围，应用自己数开着的对话框，有就解绑（`form::keys_without_dialog`）

## P10 平台与工程

- [x] 字体：Pango 自己做回退，已确认中文各字重、粗体正常
- [x] 测试：用 nsh-testing 无头驱动窗口（读信、写信、设置、文件夹），对着假 IMAP / SMTP 服务器（`src/tests/`）。
      外壳在测试里换成无头的工具包（`shell/headless.rs`）：每个窗口一个 `Headless`，开写信窗口、关窗、退出、
      常驻都走真实的外壳代码。web view 里的正文、画出来的样子不在其内（docs/architecture/testing.md）
- [x] 在主 workspace 里：longbridge/nsh#966 在上游 main 修好（#1022），曾用 git 依赖钉在 main 的 commit 上。
      nsh 0.6.0 起声音（GStreamer）、HTTP、WebSocket、SVG 都是可选的 feature，只打开 `webview` 和画图标的
      `svg`，开发 shell 和 CI 不再装 GStreamer 的开发包
- [x] 换回 crates.io：nsh 0.5.0 包含之前钉的所有修复，仓库已移出 `deny.toml` 的 `allow-git`
- [x] cargo deny：许可证（target-lexicon、winx 的 LLVM-exception）在
      `deny.toml` 里逐个放行；nsh-updater 换成 zip 9（longbridge/nsh#1164）后，zip 8 的四个 advisory 不再忽略
- [x] GTK 4.22 上 `gtk_widget_measure` 警告刷屏：`NshView::size_allocate` 不 measure 就 allocate 子 widget，上游
      longbridge/nsh#1048 已由 longbridge/nsh#1049 修好，nsh 0.5.0 已包含（已在 GTK 4.22.4 上确认无警告）
- [ ] 内存、滚动帧率、冷启动时间的基准
- [x] **Linux 的安装包**：图形界面打成 Flatpak（`packaging/linux/`），用 GNOME runtime 带上 GTK 4 和 WebKitGTK，
      发布时随 CLI 一起发 `.flatpak` 包（docs/architecture/packaging.md）
- [ ] 上 Flathub
- [x] macOS 后端（AppKit，正文在 WKWebView）：`shell/` 分成共用的部分和两个工具包（`gtk.rs`、`appkit.rs`），窗口、
      退出流程、常驻、设置和主题两边共用。⚠️ AppKit 没有 GLib 那样的主循环 future 执行器，用 `async-task` 在主
      dispatch queue 上跑；输入框失焦自己用事件 monitor 做（同 Linux）。菜单栏、外观映射抄了 nsh `run_app` 的私有
      实现（停主循环原来也抄了，现在退出都经过 AppKit 的 `terminate:`，用不着了，#366），失焦两个后端各一份，宜由 nsh 提供（#248）。菜单栏有应用菜单（⌘Q 走退出流程）、Edit、Window（⌘W
      走窗口的关闭询问），点 Dock 图标唤回主窗口；信号用 `ctrlc`。快捷键提示用 nsh 的 `key_label`
- [x] macOS 状态栏图标（frontend 的 `tray::macos`）：计数、绘制与 Linux 共用（`tray/mod.rs`），模板图用 resvg 画一次，
      未读数写在图标旁和 Dock 角标上
- [x] macOS 窗口外框（`chrome.rs`，nsh 的 `TitleBarStyle::Hidden` 和 `window_drag`）：主窗口一体式，红绿灯在侧栏
      左上角、各栏顶部那一行里（52 pt，AppKit unified 工具栏给按钮的那一行高；主窗口通过 `controls_offset` 居中并
      与侧栏 12 pt 左边距对齐，写信窗口由 nsh 居中；高度从 `title_bar()` 读，longbridge/nsh#1068 已修），这一行按下
      拖动窗口。写信窗口保留标题栏，但是应用自己画的
- [x] macOS 磁盘映像打包这个 `catmail-gui`（`packaging/macos/build-dmg.sh`）：菜单栏显示应用名、Dock 有图标。
      `cargo run` 起的没有 bundle，仍显示可执行文件名
- [x] macOS 新邮件通知：⚠️ nsh 的通知要经过视图的根，常驻时没有视图，所以应用直接用 `UNUserNotificationCenter`，
      自己做 delegate（`new_mail/macos.rs`）。只在 bundle 里有，`cargo run` 起的没有
- [x] macOS：注销时不取消。`applicationShouldTerminate` 答 `TerminateLater`，写信窗口存完再答可以退出，AppKit 自己
      结束进程，引擎在 `applicationWillTerminate:` 里关；有写信窗口关不了时答不退出，注销随之取消
- [ ] **Windows**：接上 nsh 的 Windows 后端（`nsh-platform-windows`：Win32、Direct2D、DirectWrite、正文在 WebView2）。
      要做的是外壳的第三个工具包（窗口与主循环、信号与退出）、托盘（frontend 的 `tray` 里加 Windows 的实现，#109）、
      新邮件通知；单实例已在 `catmail_frontend::instance`，三个平台一套。再加 Windows 的打包。之前 Windows 上只有 CLI
