# catmail-nsh roadmap

目标：功能逐项对齐 `catmail-gui`（GPUI）。行为以 [docs/architecture/gui.md](../../docs/architecture/gui.md)
和 gpui 版的实现为准，引擎不动。每项做完勾上，并在 `.agents/note/` 留记录。

**做法**

- **一项一提交**：对齐一项就提交一次，附上与 gpui 版的截图对比。
- **纯逻辑共用，不复制**：gpui 版里不依赖窗口的纯逻辑先抽进 `catmail-frontend`，再从 nsh 版调用。包括文件夹分区、
  事件刷新的节流、日期格式、HTML 策略、设置文件、主题推导与导入。
- **注明替换方案**：nsh 没有对应能力、需要换做法的地方标 ⚠️。

## P0 只读三栏（已完成）

- [x] 文件夹、邮件列表（虚拟列表、滚到底取下一页）、读信区
- [x] 正文在 WebView 里：CSP 断网，内嵌图片嵌成 `data:`，页面 JS 关闭，注入脚本回报高度和链接
- [x] 暗色重新着色（`recolor_html`）
- [x] 收到引擎事件后刷新（250 ms 内合并成一次）

## P1 应用骨架

- [x] 自建 `gtk::Application`，每个窗口放一个 `AppView`，替换 `run_app`（`shell.rs`）。引擎由应用持有（`engine.rs`），
      主循环结束后 `shut_down` 并等它关好；Ctrl+Q、Ctrl+W、SIGINT / SIGTERM 都走这个退出流程
- [x] 新建 `catmail-frontend`，抽出 `instance`、`settings`（数据和文件读写）、`format`、`html`。
      其余的到用到时再抽：`folders`、`mailbox`、`refresh` 在 P2，`theme/` 的推导和导入在 P7
- [x] 引擎异步打开（`open_async`）：先显示窗口，打不开时显示原因（文案与 gpui 版相同）
- [x] 单实例（`catmail_frontend::instance`）：再次启动时唤起已有窗口，与 gpui 版互通（谁先启动谁是那个应用）；
      Wayland 下用启动方的 activation token 获得焦点
- [x] 日志写进数据目录（`logging::write_file`）
- [x] 读同一份 `settings.json`，应用外观和文字大小（`AppView::set_font_scale`）；写入随 P7 的设置对话框一起做
- [x] 图标：gpui 版的 Heroicons SVG，`include_str!` 后用 nsh 的 `svg` 画，`currentColor` 跟随文字色（`icons.rs`，随 P2 做）

## P2 侧栏（gui.md「侧栏」）

模型都在 `catmail-frontend`（`folders`、`mailbox`、`refresh`），与 gpui 版共用；nsh 版只有界面（`sidebar.rs`）。

- [x] All Inboxes（多账户时显示，默认选中；`list_inboxes`）
- [x] 有角色的文件夹排前面，顺序固定，显示角色名，带图标；从嵌套里提到账户这一层（`folders::AccountFolders`）
- [x] Gmail 的"Labels"单列一节；"所有邮件"被隐藏时显示提示
- [x] Drafts 是本机草稿；Outbox 只在有待发时出现，有问题时带警告图标（警告色是应用自己的 token，随 P7）。两者的列表随 P3 的来源一起做，现在是空的
- [x] Sent、Junk、Trash 不显示未读数
- [x] 底部一行说明连接问题（`account_status`），截断时悬停显示全文
- [x] 侧栏一次读完（`overview`）；`DraftsChanged` 只重数该账户的草稿（`count_drafts`、`DraftCounts`）。
      引擎事件按 `Refresh::after` 决定重读什么，200 ms 节流（`Throttle`），读侧栏一次一个（`Coalesce`）
- [x] 右键菜单：新建、重命名、删除文件夹和标签；F2 重命名（docs/architecture/folders.md「界面」）。
      菜单是原生的 `popup_menu`，删除确认是原生的 `alert_dialog`，命名用 `dialog()` 加输入框
- [x] 打开文件夹时同步它（`sync_folder`）
- [x] 账户可以折叠：只有一个账户时默认展开，多个时默认折叠，选中某处时展开它的账户
- [ ] 子文件夹折叠（gpui 版每个有子文件夹的行可以收起）；折叠的动画
- [x] 底部的 Settings 按钮（随 P7）
- [x] Sync 按钮（随 P9）：在搜索框左边，同步时转圈（nsh 的循环动画）

## P3 邮件列表（gui.md「邮件列表」）

行的模型在 `catmail-frontend`（`list`：来源怎么分页读、按天分组、重读后选中去哪、按标记勾选、行上的文字；
`mailbox::list_title`、`list_note`），与 gpui 版共用；nsh 版只有界面（`list.rs`）。

- [x] 按天分组，标题用 Today、Yesterday 等。⚠️ nsh 列表的 sticky header 只能在建表时给一次，分组随数据变，
      所以标题是列表里的普通行，不吸顶（gpui 版的 section 标题也不吸顶）
- [x] 两种行形：读信区关着时一行排开，打开后两行；切换时把选中的那封滚到中间。nsh 列表按行量高度，
      两行时主题按实际行数占高，不必像 gpui 版那样总占两行
- [x] 头像（姓名首字，底色按地址挑）；未读只用字重表示；Gmail 用户标签小标签（最多两个）
- [x] 悬停的行显示 To 或 Cc
- [x] 悬停的行显示操作按钮（一行排开时，跟在主题后面）
- [x] 勾选多封：点头像或按 X 勾选；标题的勾选框和按标记勾选的菜单（原生 `popup_menu`）
- [x] 有勾选时标题旁的说明换成批量工具栏
- [x] 搜索：200 ms 防抖；先显示本地结果，1 s 后再问服务器（`search_server`）；选侧栏时清空搜索
- [x] 刷新规则对齐 `refresh::Refresh::after`：200 ms 节流、合并请求、结果没变不替换、按行的身份保持选中；
      选中的邮件消失时清空读信区；消失的邮件不再勾选
- [x] 来源：草稿、发件箱（它们的右栏随 P5、P6）
- [x] Escape 关上读信区

## P4 读信与操作（gui.md「读与改」「读信」）

命令的模型在 `catmail-frontend`（`commands`：`Applies`、命令的名字、归档 / 移动作用于哪些邮件、"Move to"列出的
文件夹、永久删除的确认文案、被拒改动的说明；`list::Removal`：移走后等它离开；`reader::Header`：头部），与 gpui 版
共用；nsh 版只有界面（`commands.rs`、`reader.rs`、`toast.rs`）。

- [x] 工具栏：返回；Archive、Delete、Move to、Labels（Gmail）；已读 / 未读、Flag；Show remote images、Open in browser
- [x] Reply、Reply all（`can_reply_all`）、Forward（Ctrl+R、Ctrl+Shift+R、Ctrl+Shift+F）
- [x] 读信区工具栏、勾选工具栏、悬停的行用同一套按钮（`commands::group`）；快捷键 E、Delete / Backspace、L、
      Ctrl+Shift+M / U / L，作用于勾选的邮件，没有勾选时是选中的那封
- [x] 打开即已读（读 `settings.json` 的 `mark_read_on_open`）；只在用户选中、或移走后打开下一封时标记
- [x] 归档、删除、移动、去掉标签后打开下一封（`Removal`：`expect`、`settled`）
- [x] 永久删除先确认（`DeleteOutcome`），原生 `alert_dialog`
- [x] "Move to"、"Labels"对话框：输入即过滤，上下键和 Enter 或点击选；Labels 的勾选跟着列表里这封邮件的标签，
      选中的不再是它时关掉
- [x] 服务器拒绝的改动用通知说明（`ChangesRefused`、漏收事件、窗口打开时；`dismiss_settled`，一次一查）。
      ⚠️ nsh-base 只有无样式的 toast 原件，通知的样式和计时在 `toast.rs`；出错不再写在侧栏底部，也改成通知
- [x] 远程图片：先显示缓存里的（`cached_remote_images`）；点击后用 `fetch_remote_images` 逐批显示，换邮件即取消。
      ⚠️ 不重新加载页面：注入脚本收到 `post_message` 后把 `data:` URL（`catmail_core::html::data_url`）填进对应的
      `<img>` 和 `background`；页面因外观变化重新加载时（脚本报 `r`）再发一遍。CSP 照样只放行 `data:`
- [x] 附件列表；点击后弹系统保存对话框（`pick_save_path`、`safe_filename`），写文件在另一个线程
- [x] 头部：头像；收件人一行只列名字；"Details"展开完整头部；Gmail 用户标签
- [x] 在浏览器打开（`hand_off`，`open_path`）
- [ ] 正文的查找：WebKit 没有自带的查找栏，nsh 也没给 `WebKitFindController`，要自己用注入脚本做。
      选中、复制、全选是 WebKit 自带的
- [ ] 打开时先显示文字、内嵌图片后到（现在等内嵌图片取完才建页面：后到就要重新加载页面）

## P5 写信窗口（gui.md「写信窗口」，依赖 P1 多窗口）

写信的模型在 `catmail-frontend`（`composer`：`ComposeFrom::start`、From 的选项、字段解析、`Lifecycle` 决定何时
保存 / 发送 / 关窗、`OpenComposers` 登记窗口和退出前全部关闭），与 gpui 版共用；nsh 版只有界面和窗口
（`composer.rs`、`draft.rs`，`shell.rs` 里的 GTK 窗口）。每个 nsh 视图跑在自己的引擎线程上，写信窗口的视图和
GTK 线程之间用 `shell::on_gtk` 与一个关窗请求的通道来回。

- [x] 每封草稿一个窗口；同一草稿只开一个（`OpenComposers`）
- [x] 一开始就是草稿（`create_draft`、`reply`、`forward`）；没改就关窗时删掉
- [x] 停顿 800 ms 自动保存；同一草稿的引擎调用不重叠；发送、关闭、丢弃前做最后一次保存（`Finish`）
- [x] 地址栏：解析和显示规则（`parse_addresses`、`format_addresses`），关窗前检查
- [x] From 下拉（`draft_senders`，原生 `popup_menu`）；身份变化时重读身份和草稿
- [x] 附件：选文件或拖进窗口（nsh file-drop）
- [x] Ctrl+Enter 发送；关窗按钮、Ctrl+W 走同样的流程：GTK 的 `close-request` 总是拦下，交给视图决定，视图存好后
      自己销毁窗口
- [x] 退出（Ctrl+Q、SIGINT / SIGTERM）前先关所有写信窗口（`close_all`），有一个关不了就不退出
- [x] 草稿的右栏：Edit、Delete draft；双击或 Enter 打开草稿的写信窗口；侧栏顶上的 Compose（Ctrl+N）

## P6 发件箱（gui.md「发件箱」）

右栏的模型在 `catmail-frontend`（`outbox`：`State` 说条目怎样了，`choices` 和 `Choice` 给出可选的操作、措辞、
要先确认的和对应的引擎调用），与 gpui 版共用；nsh 版只有界面（`outbox.rs`）。

- [x] 右栏按条目状态给出操作：Send now、Back to drafts、重试、Mark as sent、Send again（`OutboxStatus::can_*`），
      可能重复的 Send again 先用 `alert_dialog` 确认
- [x] 后台发送（`send_in_background`，P1 起就开着）；`unknown` 的条目从不自动重发（引擎的规则）

## P7 设置与主题（gui.md「主题与设置」）

主题的推导、导入和主题目录在 `catmail-frontend`（`theme`：主题是 gpui-kit 主题 JSON 的格式，但不依赖 gpui-kit），
Accounts 的确认文案在 `accounts`，与 gpui 版共用；nsh 版把 gpui-kit 的颜色键映射成 shadcn 的 token（`theme.rs`），
设置和主题是进程级的（`shell.rs`），对话框在 `settings.rs`。

- [x] 设置对话框：外观、浅色和深色各自的主题、文字大小（`AppView::set_font_scale`）、打开即已读；Ctrl+,，
      Ctrl + / Ctrl − / Ctrl 0 改文字大小。改了就写回 `settings.json`，所有窗口（包括写信窗口）随之更新：
      主题和文字大小在 GTK 线程上设给每个 `AppView`，外观由各视图自己 `set_color_scheme`
- [x] Reading：远程图片缓存的大小和"Clear image cache"
- [x] Accounts：账户列表、Add account；发件身份的默认、改名、删除、"Server saves to Sent"、Add sending address
- [x] 主题：gpui-kit 的颜色键映射到 shadcn 的 token，再加上应用自己的（`warning`、`link`、六个色相等），经
      `set_theme_overrides` 给每个视图；旗标、头像、徽章、发件箱的警告不再写死颜色。导入各种终端配色格式；主题目录
      与 gpui 版是同一个
- [x] 跟随系统外观（nsh 自带，经 XDG portal）
- [x] General（关窗后常驻、新邮件通知），随 P9

## P8 登录页（gui.md「登录页」）

字段怎么读、端口的默认值和提交的状态在 `catmail-frontend`（`login`：`AccountValues`、`IdentityValues`、
`Submission`），与 gpui 版共用；nsh 版的字段和页面在 `login.rs`，对话框外框与命名对话框在 `form.rs`。

- [x] 没有账户时自动打开；`add_account(.., Verify::Login)`；进行中不能取消
- [x] 表单字段与发件身份的表单共用；任一字段按 Enter 提交

## P9 常驻与系统集成（docs/architecture/background.md）

托盘的菜单、提示文字、带角标的图标 SVG 和 Linux 的 StatusNotifierItem（`tray`）、新邮件通知的攒批、措辞和 tag
（`new_mail`）、点击后的去处（`mailbox::Destination`）、关窗后的去留（`resident`）、`malloc_trim`（`memory`）、
列表等待某封邮件出现（`list::Reveal`）都在 `catmail-frontend`，与 gpui 版共用。nsh 版只做计数、绘制和收发
（`tray.rs`、`new_mail.rs`），常驻与退出在 `shell.rs`。

- [x] 托盘：ksni（frontend 的 `tray::linux`），显示未读数，菜单是 Open catmail、New message、Sync now、Quit。
      图标用 resvg 在另一个线程上画（nsh-core 已经带了 resvg，这里另外打开 `text`，用来画角标数字）。亮暗跟随
      portal 的 `color-scheme`，即桌面的外观而不是应用的；常驻时也跟随（gpui 版要等下次开窗口）
- [x] 关窗后常驻：最后一个窗口关掉时 `app.hold()`，重开窗口时放掉。重开时回到上次的位置（`shell::left_at`）。
      Quit（Ctrl+Q、托盘、SIGHUP / SIGINT / SIGTERM）先关写信窗口，第二个信号直接退出；日志记
      `quitting reason=…`。⚠️ GTK 常驻时仍连着显示服务器（gpui 版会断开）
- [x] 新邮件通知：⚠️ nsh 没有通知，改用 GLib 的 `GNotification`。点击走应用的 `open-mail` action，打开对应账户的
      Inbox，列表读到那封后选中它（`Reveal`）。GNOME Shell 上点击无效：GLib 在那里改用 GNOME 自己的接口，它经
      D-Bus 名字唤起应用，而 nsh 版不占这个名字（`NON_UNIQUE`，单实例靠 socket）
- [x] 系统唤醒：nsh 不听 logind，靠引擎自己发现睡眠（`sleep.rs`，最多晚 30 秒）
- [ ] 常驻时回收内存：关掉最后一个窗口 2 秒后 `malloc_trim` 已接上；还没量 WebKit 子进程常驻时的占用
- [x] 快捷键对齐 `actions.rs`。这次补上的有：Sync（Ctrl+Shift+N、F5）、Ctrl+F 聚焦搜索框、写信窗口的 Escape 关闭
      和 Ctrl+Shift+A 加附件

## P10 平台与工程

- [ ] 字体：Pango 会自己做回退，`fonts.rs` 里那些变通预计用不上。要验证中文各字重、粗体是否正常
- [ ] 测试：用 nsh-testing 无头驱动窗口，对应 gpui 版的 `mailbox/tests/*`（读信、写信、设置、文件夹）
- [ ] 回到主 workspace：等 longbridge/nsh#966 修好
- [ ] CI：Linux job 装 GTK4 和 WebKitGTK；cargo deny 的许可证（CDLA-Permissive-2.0、LLVM-exception）和 bincode advisory
- [ ] GTK 4.22 上 `gtk_widget_measure` 警告刷屏
- [ ] 打包：`.desktop`；运行时依赖 GTK 4.14+ 和 WebKitGTK 6.0
- [ ] 与 gpui 版对比：内存（含 WebKit 子进程）、滚动帧率、冷启动时间
- [ ] macOS（WKWebView）、Windows（WebView2）后端
