# GUI

[← 架构](../architecture.md)

`catmail-gui`，基于 longbridge 的 [Native Shell](https://github.com/longbridge/nsh)（nsh）：界面是各平台自己的
控件，Linux 上 GTK 4 加 Pango，macOS 上 AppKit；布局用 Taffy，状态是 SolidJS 式的 signal。邮件正文放在
nsh 的 web view 里（WebKitGTK 6.0、WKWebView）。nsh 也有 Windows 后端（`nsh-platform-windows`：Win32、
Direct2D、DirectWrite、WebView2），`catmail-gui` 还没接上它：Windows 上照样编译，启动时说明不支持后退出，
Windows 用户目前只有 CLI。

与 UI 库无关的模型都在 `catmail-frontend`（侧栏、列表、命令、写信、发件箱、登录表单、主题、设置、托盘、通知
的措辞……），这个 crate 只有视图和外壳。nsh 用 git 依赖钉在上游 main 的一个提交上（根 `Cargo.toml`），发版后换回
crates.io。逐项的进度、未做的事和 nsh 的变通（⚠️）见 [`crates/catmail-gui/ROADMAP.md`](../../crates/catmail-gui/ROADMAP.md)。

## 选型

起初 GUI 基于 GPUI 和 gpui-kit，正文由 Blitz（`catmail-render`）在进程里排版、画成像素。之后用 nsh 写了一个
试验前端，逐项对齐到同样的功能，2026-11 起它取代了 GPUI 版，GPUI 版连同 `catmail-render` 删掉了：

- **开发体验更好**：原生控件、signal 和 Taffy 布局；文字、输入法、选择、复制、菜单、对话框、文件选择都是
  系统的，不用自己做。GPUI 版为字体（Linux 上可变字体的字重、CJK 回退）、文字选择、菜单焦点做过大量变通。
- **没有卡住我们的问题**：nsh 缺的（通知、查找栏、部分 AppKit 集成）都能在应用里补上，见 ROADMAP 的 ⚠️。
- **正文用浏览器内核，兼容问题少**：邮件 HTML 是给浏览器写的，Blitz 还在 beta，表格布局、旧式颜色属性都要
  在 fork 里修。代价是 WebKit 自带网络，断网改由页面自己做（见[读信](#读信)），以及每个窗口一个 WebKit 渲染
  进程的内存。
- GPUI 版评估过嵌入 WebView 并放弃：WebKitGTK 不能嵌进别人的 Wayland surface，整个应用要跑在 XWayland 上。
  nsh 的 Linux 后端本身就是 GTK，web view 是普通的 GTK 控件，这个问题不存在。

模型早已抽进 `catmail-frontend`，换 UI 库时没有动它们：GPUI 版和 nsh 版共用过同一份。

## 结构

按窗口和区域分目录，仿照原来 GPUI 版的划分：主窗口的各栏在 `mailbox/`，读信区、写信窗口、设置对话框各一个目录，
大的视图再把"状态和行为"与"长什么样"（`view.rs`）分开；各处共用的小部件放在 `src/` 下。

```
src/
  main.rs          启动：交给已在运行的实例或成为它（catmail_frontend::instance），日志，引擎的选项，交给 shell::run
  engine.rs        引擎：属于应用，不属于窗口；打开中、已打开、失败、已关闭
  shell/           外壳：mod.rs 窗口、设置与主题、托盘和通知的请求、常驻与退出；gtk.rs、appkit.rs 两个工具包
  mailbox/         主窗口
    mod.rs         引擎打开前留空、失败的原因、登录页或三栏、引擎事件的刷新
    panes.rs       三栏和拖宽度的把手，右栏按选中的是邮件、草稿还是发件箱条目；右栏共用的返回按钮、字段行
    sidebar.rs     侧栏：All Inboxes、账户与文件夹、标签、连接说明
    folder_commands.rs  文件夹的右键菜单：新建、改名、删除
    list/          中栏：mod.rs 发起读取并交给 frontend 的 ListModel、勾选、搜索；rows.rs 日期标题和三种行
    commands.rs    对邮件的命令：图标按钮组、快捷键、被拒改动的通知
    picker.rs      "Move to"与"Labels"对话框
    draft.rs outbox.rs  草稿、发件箱条目的右栏
  reader/          读信区：mod.rs 打开、远程与内嵌图片、附件的保存；view.rs 工具栏、头部、正文的 web view、附件
                   （正文的页面：CSP、注入脚本和它的消息协议在 catmail_frontend::html）
  composer/        写信窗口：mod.rs 状态、保存发送关闭、附件、发件人；view.rs 窗口
  settings/        设置对话框：mod.rs 设置的跟随、对话框和 General；appearance.rs、accounts.rs、reading.rs 各一节
  login.rs         登录页和发件身份的表单
  form.rs          对话框外框、字段、提交、确认对话框、只要一个名字的对话框
  tasks.rs         视图开始的工作归谁（随窗口结束），另起线程的工作
  buttons.rs       图标按钮（工具栏、行两种尺寸）、主按钮、文字按钮、分隔、提示
  chrome.rs        窗口顶部：macOS 的一体式主窗口、写信窗口的标题栏、各栏顶部的工具栏
  theme.rs         主题映射到 nsh（shadcn）的 token
  clock.rs         系统的 12 / 24 小时设置
  toast.rs         窗口里的通知；report 记日志并提示出错
  icons.rs         Heroicons
  tray/            托盘：mod.rs 计数、把图标画成像素（何时数、何时画由 frontend 的 TrayModel 定），linux.rs、
                   macos.rs 交给 frontend 的 StatusNotifierItem、状态栏项
  new_mail.rs      新邮件的系统通知（Linux）
```

## 外壳

nsh 的 `run_app` 一个视图一个窗口，窗口关了应用就结束。我们要多个窗口（写信窗口）、关掉主窗口后常驻，所以
自己运行工具包的应用（`shell/`）：Linux 上是 `gtk::Application`，macOS 上是 `NSApplication`。窗口、退出流程、
常驻、设置和主题两边共用（`shell/mod.rs`），工具包的差别在 `gtk.rs`、`appkit.rs`。

- **线程**：外壳在主线程（工具包的）上；每个窗口一个 `AppView`，它的视图跑在 nsh 自己的引擎线程上。视图经
  `shell::on_main` 请主线程做事（开窗、关窗、改设置），主线程经通道回话（写信窗口的关闭请求）。
- **引擎**由应用持有（`engine.rs`）：第一个窗口先显示，引擎在主循环上 `open_async`，视图用 `engine::opened`
  等它；打不开时窗口显示原因。主循环结束后 `shut_down` 并等 `closed()`，这时主线程上已经没有别的事。
  门面的 future 在 nsh 的任务里（UI 线程上的本地 executor）await，工作在引擎自己的 runtime 上做。
- **macOS** 没有 GLib 那样的主循环 future 执行器：主线程的 future 用 `async-task` 跑在主 dispatch queue 上。
  菜单栏有应用菜单（⌘Q 走退出流程）、Edit、Window（⌘W 走窗口的关闭询问），点 Dock 图标唤回主窗口
  （`applicationShouldHandleReopen`）。`applicationShouldTerminate` 先答取消，等写信窗口存完再退出循环。
  菜单栏、停主循环、外观映射抄了 nsh `run_app` 的私有实现。
- **输入框失焦**：在输入框外按下时把焦点从输入框拿走，像浏览器那样，否则作用于邮件的单键（E、X、L）到不了
  视图。nsh 把焦点留在输入框里，两个后端各用一个事件 monitor 补上（应由 nsh 提供，#248）。
- **单实例**：GTK 应用是 `NON_UNIQUE`，不经 D-Bus 判断唯一；一个数据目录一个应用由引擎的锁和 `gui.sock`
  决定（见[后台运行](background.md#单实例与唤起)）。
- **app id** 是 `net.yangkx.catmail`（`shell::APP_ID`）：Wayland 的 app id、X11 的 `WM_CLASS`、`.desktop`
  文件名、macOS 的 bundle id。

### 窗口外框（macOS）

`chrome.rs`。macOS 上主窗口没有系统标题栏：它只写着窗口名，占一行又不提供什么。Linux 上仍用系统的标题栏。

- **主窗口是一体式**（像 Mail.app，nsh 的 `set_title_bar(Hidden)`）：内容一直到窗口顶部，红绿灯在侧栏左上角。
  顶部一行就是红绿灯所在的行（52 pt，AppKit unified 工具栏的高度）：右栏的工具栏和中栏的同步、搜索一行都是
  这个高度、互相对齐，侧栏顶部空出这一行。这一行按下拖动窗口（`window_drag`）。全屏时红绿灯隐藏，这一行回到
  平常的 48 pt。登录页、打不开引擎的页顶部空出这一行（`chrome::page`）。
- nsh 的 `title_bar()` 在 macOS 27 上报 66 pt，比按钮所在的行高（longbridge/nsh#1068），所以只用它判断有没有
  红绿灯，高度用常量。
- **写信窗口保留标题栏**，但是应用自己画的：侧栏的颜色，标题（信的 Subject）居中。系统的标题栏配不上导入
  主题的颜色。设置是主窗口里的对话框，没有自己的窗口。
- 窗口标题仍然设置（窗口菜单、Mission Control、辅助功能读它），只是不显示。

## 状态与事件

状态放在 nsh 的 global 里（每种类型一个 signal，每个窗口一份）。主窗口的任务都属于邮箱
（`tasks::own_tasks`、`in_view`）：一行的点击开始的工作比这一行活得久，随窗口一起结束。选中文件夹或邮件经
`effect` 起任务，选中变了，effect 重新运行，还在加载的旧任务随之取消。

客户端开着后台同步、后台发信、预取正文和 GC。GUI 自己不同步账户：Sync 按钮（Ctrl+Shift+N、F5）和托盘的
"Sync now"调 `MailClient::sync_now`，由后台同步来做（见[同步 / 后台同步](sync.md#后台同步与-idle)）；有账户
`syncing` 时搜索框旁的 Sync 按钮转圈，转完当前一圈才停，失败时侧栏底部显示连接问题。后台同步在两次完整
同步之间只盯 INBOX，所以打开一个文件夹时同步它（`sync_folder`，见[同步 / 打开的文件夹](sync.md#打开的文件夹)），
失败只记日志。

哪个事件重读什么由 `catmail_frontend::refresh::Refresh::after` 一张表决定：

- **侧栏**一次读完：`MailClient::overview` 在一个读事务里取所有账户的文件夹（含计数）、草稿数和发件箱。
  连接状态不在数据库里，结果落到窗口时才取 `account_status`，不会比它的事件旧。账户增删、`FoldersChanged`、
  `MessagesChanged`（计数）、`OutboxChanged` 重读侧栏。
- **草稿数**：写信窗口每次停顿都保存，`DraftsChanged` 只重数该账户的草稿（`count_drafts`）。它和侧栏的读取
  没有先后，所以按账户记下收到的次数（`DraftCounts`），开始得比最近一次事件早的读取不改这个账户的草稿数。
- **列表**：`MessagesChanged` 且列表显示这个文件夹（All Inboxes 是其中一个收件箱），或显示搜索结果（任何文件夹
  变了都可能影响结果）；账户增删、`FoldersChanged` 也重读搜索结果和 All Inboxes；侧栏读完后列表显示的文件夹
  变了也重读（账户第一次同步时邮件的事件可能早于侧栏读到这个 INBOX）；`DraftsChanged` / `OutboxChanged` 且
  显示该账户的草稿 / 发件箱。重读已显示的行，按行的身份保持选中；选中的邮件不见了（被删除、UIDVALIDITY
  改变）时清空读信区，用户自己移走的除外（见[读与改](#读与改)）。
- `AccountStatusChanged` 直接更新连接说明；`ChangesRefused` 报告被拒绝的改动；`PendingOpsChanged` 什么也不触发。
  `Lagged` 时整体刷新（侧栏、列表，并查一次被拒绝的改动）。
- **节流**（`Throttle`）：一次读取之后 200 ms 内的事件攒着，间隔结束时合在一起读一次。单个改动（标记已读）
  马上显示，一次同步的几十个事件每 200 ms 最多读一轮。节流之下，读侧栏和列表各自还合并请求（`Coalesce`）：
  同一时间最多一次读取，期间再有事件只记下，完成后补读一次；已交给引擎的读取不会因为换了任务而不做。
- 读回来的和正显示的一样时不替换：同步每批邮件发一个事件，多数轮次什么都没变。
- **分页**：文件夹和搜索结果每页 200 条，滚到底再取下一页，按游标取排在最后一行之后的（见
  [存储](storage.md#列表顺序与分页)）：顶上新来或删掉的行不会让下一页重复或漏行。

## 侧栏

模型在 `catmail-frontend`：文件夹的层级在 `folders`，地方（`Place`：名字、图标、列出哪些文件夹）在 `mailbox`，
侧栏的分组和排序、折叠（`sidebar_groups`、`Folds`）和读回账户之后做什么（`SidebarModel`：一次一个读取、保留更新的
草稿数、窗口打开前托盘或通知要求去的位置和写信、原来的位置不在了时回到开始处、列表显示的文件夹变了时重读列表）
在 `mailbox::sidebar`，都有表驱动测试。`mailbox/sidebar.rs` 只把模型的结果放进 nsh 的 signal 再画出来。分组的
子树用 `Rc` 共享，账户每变一次只建一次，传给各层时不再整棵复制。

有多个账户时第一项是 **All Inboxes**（只有一个账户时不列，它就是那个账户的 Inbox）：每个账户的 INBOX（与引擎
同样按 `special_folder` 挑；Gmail 上是 INBOX 标签）合成一个列表，`MailClient::list_inboxes` 按与单个文件夹相同
的顺序和游标分页。计数是各收件箱未读之和。打开窗口时默认选中它，原来的位置不在了时也回到这里；选中时同步
每个收件箱。在这里移动 Gmail 的邮件，看作是在它账户的 INBOX 里看的，移到别的标签会去掉 INBOX。

每个账户先列有角色的文件夹，顺序固定（Inbox、Drafts、Outbox、Sent、Archive、All Mail、Flagged、Junk、Trash），
名字用角色名（`FolderRole::label`）而不是服务器上的名字；不论嵌套在哪（Dovecot 的 `INBOX.Sent`）都提到这一层，
原父级没有其他内容时去掉（`folders::AccountFolders`）。其余文件夹按服务器的层级跟在后面。

- **Gmail 的用户标签单列一节**"Labels"。"所有邮件"在 IMAP 中隐藏时，账户名下面一行说明并告诉用户怎样恢复
  （见[位置与标签 / 界面](labels.md#界面)）。
- **Drafts 是本机草稿**，不是服务器的 Drafts 文件夹：那里是本机草稿的副本，按邮件列出会重复、也不能编辑。
  代价是其他客户端写的草稿在 GUI 里看不到（CLI 的 `catmail ls Drafts` 能看到）。
- **Outbox 只在有待发邮件时出现**，有被拒绝或未确认的条目时带警告图标。
- Sent、Junk、Trash 不显示未读数。
- **折叠**：只有一个账户时默认展开，多个时默认折叠，选中某处时展开它的账户；有子文件夹的行可以收起。
- **右键菜单**新建、重命名、删除文件夹和标签，F2 重命名选中的（见[文件夹与标签的管理](folders.md#界面)）。
  菜单是原生的 `popup_menu`，删除确认是原生的 `alert_dialog`。
- 顶部是 Compose（Ctrl+N），底部是 Settings 和一行连接问题（登录被拒、超出下载限额、连不上、发不出去；
  发不出去时列出每个发不出的地址），截断时悬停显示全文。
- 选中只改颜色、图标换成填充的，不改字重；设置对话框的导航用同一种行。

## 邮件列表

行的模型在 `catmail_frontend::list`（来源怎么分页读、按天分组、重读后选中去哪、按标记勾选、行上的文字）。
列表显示什么是 `Listing`：行、按天的分组和排成的条目，条目只记日期标题和行的下标，翻页时只追加，不再把每行
复制一份。怎么读是 `ListModel`：换来源时作废旧来源的读取、一次一个读取（`Coalesce`）、搜索框停顿后换成什么（`typed`、
`paused`：期间又有按键就不搜，没输入就回到选中的地方，和正显示的一样就不动）、什么时候问服务器（`server_search`，
用户已经离开这次搜索就不问）、搜索时带上服务器找到的、
读完后按 `Removal`、`Reveal`、`Reselect` 定下选中哪一行、哪些勾选不在了、要不要再读一次（`loaded`），都有表驱动
测试。`mailbox/list/` 发起读取、把结果交给模型，再把 `Listing`、选中、勾选和"正在读"放进 nsh 的 signal；
"正在读"单独一个 signal，同步时每次刷新不再让读行的地方重算。

- **按天分组**：列表（都是新的在前）按本地日期分组，标题是 Today、Yesterday、今年的"Monday, October 12"、
  更早的"October 12, 2025"，没有日期的归到 No date；行里只显示时间。只合并相邻的同一天，顺序不是按日期时
  一天会出现两次标题。⚠️ 标题是列表里的普通行，不吸顶：nsh 列表的 sticky header 只能在建表时给一次，而分组
  随数据变。
- **时间**按系统的 12 / 24 小时设置显示（nsh 的 `locale_settings()`，`clock::clock()` 映射成
  `catmail_frontend::format::Clock`），列表、读信区的头部和发件箱都是；读它会订阅，改了设置立刻跟着变。头部
  因此只存时间戳，显示时再格式化。命令行和回复里引用的那行"On … wrote"照旧用 24 小时。
- **两种行形**：读信区关着时列表独占窗口宽度，一行排开：发件人（草稿是收件人）一栏、主题、标记、时间；打开后
  改成两行：发件人和时间在上，主题在下，最多两行，颜色较淡（字重只表示未读）。nsh 列表按行量高度，两行时主题
  按实际行数占高。切换时把刚打开（或刚关上）的那封滚到中间，免得它从指针下跑开。
- **悬停**：一行排开时，指针所在的邮件行紧跟主题显示与读信区工具栏相同的命令按钮，标记和时间左边显示收件人
  （To，没有时 Cc）。悬停的行整个列表记一个，不是每行一个：一个单元格滚到别的行时不会带着指针状态。
- **头像**：发件人名字的首字，底色按地址从主题的六个色相里挑（同一个人颜色固定）。行里的头像也是勾选框。
- **勾选多封**：除了选中（读信区显示的那一封）还有一组勾选的邮件，两者互不影响。点头像或按 X（Gmail 的键）
  勾选；邮件来源的标题左边是勾选框和按标记勾选的菜单（All、Read、Unread、Flagged、Unflagged，原生
  `popup_menu`）。有勾选时标题右边换成对它们的工具栏。标记按全体算：都已读才是 Mark as unread，否则对全部设上。
  Archive 只对能归档的那些；Move to 要求都属于一个账户；Delete 中要永久删除的一起确认一次。移走、删除、归档后
  不再勾选，标记后仍勾选；不在新读到的行里的不再勾选；换来源时全部清空。
- **未读只靠字重**；**Gmail 的用户标签**是主题后面的小标签，最多两个、其余计数，不显示正在看的那个。
- **搜索**：停顿 200 ms 才搜索；本地结果先显示，1 秒后再问服务器（`search_server`），它找到本地没有的再读一遍
  （见[问服务器](storage.md#问服务器)）。Ctrl+F 聚焦搜索框，选侧栏时清空搜索。搜索框获得焦点时提前登录
  （`connect_for_search`）还没接上（ROADMAP）。
- Escape 关上读信区。

## 读与改

命令的模型在 `catmail_frontend::commands`（`Applies`、命令的名字、"Move to"列出的文件夹、永久删除的确认文案、
被拒改动的说明），都直接拿账户列表（`&[AccountEntry]`）。每个改动是一个 `Change`：改哪些邮件、向引擎要做什么
（`Op`，`Change::run` 调对应的 `MailClient` 方法）、失败时怎么说（`Change::failure`，"Couldn't {what}"，和删除
文件夹、发件箱的选项一样）、会不会让邮件离开列表（`may_remove`，改标记以外都会）、做完要不要去掉勾选
（`unchecks`，归档、移动、删除）。快捷键作用于哪些邮件是 `Listing::keyed`，只等选中的那封离开是
`ListModel::expect_removal`，一封邮件"在哪看"（Gmail 上移动会去掉的标签、行上不用再写的标签）是
`Source::shown_in`，移动和行上的标签名都用它（行上的按账户先算好一份，`shown_in_each`）。`mailbox/commands.rs`
只有一个 `run_change`：按 `Change` 去掉勾选、等选中的那封离开、运行、出错时通知；其余是画按钮。

- **三处同一套按钮**：读信区工具栏、勾选工具栏、悬停的行都由 `commands::group` 生成，只显示图标，按用途分组：
  位置（Archive、Delete、Move to）；标记（已读 / 未读、Flag）；读信区另有内容（Show remote images、Open in
  browser）和回复（Reply、Reply all、Forward）。Reply all 只在它比 Reply 多写给人时显示（`can_reply_all`）。
  哪些可用、开关显示哪一面由 `Applies` 按这些邮件算，一个工具栏的几组共用一份。
- **Labels 不在这套里**：它只改一封邮件，不是 `MessageCommand`，只有读信区工具栏在位置一组后面单独放一个按钮
  （Gmail 账户才有，`commands::can_edit_labels`），快捷键 L 也只对选中的那封（`commands::edit_labels`）；
  勾选工具栏和悬停的行没有它。要三处一致，得先定多封勾选时 Labels 怎么改（只许一封，或者对话框按"全部有 /
  部分有 / 都没有"三态显示）。
- **快捷键作用于勾选的邮件**：E、Delete / Backspace、Ctrl+Shift+M / U / L 有勾选时对勾选的，没有时对选中的
  那封；读信区工具栏的按钮只对它显示的那封；L（Labels）、回复、转发（Ctrl+R、Ctrl+Shift+R、Ctrl+Shift+F）只对
  选中的那封。
- **打开即已读**（设置可关）：只在用户选中、或移走后打开下一封时标记，刷新后恢复选中不算。
- **归档 / 删除 / 移动后打开下一封**（`list::Removal`）：命令先记下要移走的邮件，刷新后它仍是选中的那封却
  不在了，就按身份选中原来排在它后面、现在还在的第一行。邮件不一定离开：Gmail 上在"所有邮件"里移到标签、在
  Work 里归档，它还在列表里。会不会离开由存储列出邮件的规则决定，前端不预测：调用返回时改动已经提交，列表再读
  一次，邮件还在就不再等、选中不变；调用失败也不再等。每次有自己的编号，只有最近一次算数。
- 已在 Trash（或账户没有 Trash）时删除是永久的，先确认（`DeleteOutcome`）；其余可逆的操作不确认。能否归档、
  移到哪些文件夹由 `MovePlan` 判断，基于账户的文件夹列表而不是侧栏的顺序。
- **"Move to"、"Labels"对话框**：输入即过滤，上下键和 Enter 或点击选。Labels 点一个立即加上或去掉，对话框留着；
  勾选跟着列表里这封邮件现在的标签（被拒绝后撤销的、同步拉到的也一样），这封邮件不再是选中的那封时关掉。
- **服务器拒绝的改动**：`ChangesRefused` 后 `dismiss_settled` 在一个写事务里取出并忘掉失败或冲突的改动，逐条
  以通知说明，不会重复。窗口打开时和漏收事件之后也查一次。先读一下有没有，没有就不占写连接。

## 读信

读信区（`reader/`）：工具栏、标题、头部、正文、附件，在同一个滚动区里。头部的模型（`reader::Header`、附件的
名字、保存对话框的起始目录）在 frontend。

- **头部**：发件人头像；收件人一行只列名字，"Details"展开完整的 From / To / Cc / Reply-To / Date；Gmail 用户标签。
- **附件**：列出非内嵌部分，点击后弹系统保存对话框（文件名经 `safe_filename`），需要时先下载，写文件在另一个
  线程。
- **在浏览器打开**：清洗后的 HTML 写成独立页面（`embed_inline_images` 嵌进已加载的内嵌图片，CSP
  `default-src 'none'; img-src data:; style-src 'unsafe-inline'`，`no-referrer`），经 `MailClient::hand_off`
  写进数据目录的 `handoff/` 再交给系统浏览器。不带已下载的远程图片。
- **还不支持**：正文的查找。WebKit 没有自带的查找栏，nsh 也没给 `WebKitFindController`，要自己用注入脚本做。
  选中、复制、全选是 WebKit 自带的。

### 正文在 web view 里

有 HTML 部分就显示清洗后的 HTML，否则把纯文本转义后放进 `<pre>`（`catmail_frontend::html::PageImages::page`），都在同一个 web view 里。

- **断网**：WebKit 有自己的网络，nsh 只检查顶层导航，不管页面的子资源，所以由页面自己断网：`<head>` 最前面
  一条 CSP `default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:; form-action 'none';
  base-uri 'none'`（后两项不回落到 `default-src`），加 `no-referrer`。要显示的图片都先嵌成 `data:` URL。保证从
  "引擎根本没有网络"（Blitz）降成了"靠 CSP 拦"；用探针页（`<img>`、`srcset`、`@import`、`@font-face`、CSS 背景、
  `<link>`、iframe、`meta refresh`……）指向本机 HTTP 服务器验证过：开着 CSP 时零请求。
- **脚本**：页面自己的 JS 关掉（`java_script_enabled(false)`）。注入的脚本（`html::BRIDGE`）WebKit 照样运行，
  经 `window.nsh.postMessage` 回报：载入了（`r`）、页面高度（`h`）、按 web view 的高度画好了（`p`）、点击的链接
  （`o`）、收下了刚发来的图片（`d`）。读信区经 `post_message` 发回图片（`i:<url>\n<data URL>`），脚本把它填进
  `src` 规范化后等于 `url` 的 `<img>`（第一次收到图片时按 URL 建一张表，之后只查表：已填上的 `src` 是几百 KB 的
  `data:` URL，每张都重新解析一遍是 O(n²)）。只有 `<img>` 有图：清洗不保留 `background` 属性和样式里的 `url()`（见
  [正文](bodies.md)），表格布局邮件的 `<td background="…">` 背景图不显示。
- **网站数据**：web view 设成 `incognito`，页面留下的网站数据（cookie、存储、缓存）只在内存里，随 web view
  一起没了，不写进数据目录：正文是别人写的 HTML，页面的 JS 虽然关了，也不该和应用共用一份持久的网站数据。
  Linux 上是这个 web view 自己的临时 `NetworkSession`（不再建 `$XDG_DATA_HOME/<app id>/webkit`），macOS 上是
  不持久的 `WKWebsiteDataStore`。它随 web view 结束，不影响关窗后网络进程退出（longbridge/nsh#1051）。
- **页面带编号**（`<meta name="catmail-page">`），消息都附上它，读信区只认最后建的那个：空白页、上一封邮件的
  页面晚到的消息不会被当成新页面的。
- **高度**：web view 不滚动，高度跟着页面报的走；标题、头部和正文由外层一起滚。页面自己的 overflow 隐藏
  （`html { overflow: hidden }`）：macOS 上 nsh 的 `scrollable(false)` 只把滚轮交给外层，不像 Linux 那样隐藏
  页面的 overflow，页面比 web view 高出一点（`scrollHeight` 取整、横向滚动条占高度）WebKit 就会画出第二个
  滚动条。报的高度是根元素的盒子（含 body 的外边距和横向滚动条）向上取整，不用会被取整的 `scrollHeight`，
  也不用根元素的 `scrollHeight`（不小于 web view 的高度，换到短邮件时缩不回去）。比读信区宽的邮件在 body 里横向
  滚动。链接只把 `http` / `https` / `mailto` 交给系统打开（`is_openable_link`），页面里不导航。
- **一个 web view 一直用**：WebKit 每个新 web view 起一个渲染进程（约 100 MB PSS，还要建一次 bwrap 沙箱），
  最后一个没了就结束它。所以读信区和它的 web view 随主窗口一直存在，换邮件只往里载入新页面；没选中邮件时
  读信区放在列表下面、web view 里放一个空白页（`html::blank_page`），开窗时渲染进程就先起好。开窗后点开第一封，
  内容约 2 帧就出现（原来约 13 帧）。⚠️ 不用 `invisible` 藏起来：一阵没画的 web view 再画时，WebKit 画好之前
  先闪一帧黑；也不能不排版，nsh 只让有尺寸的原生控件可见，WebKit 只往可见的 web view 里载入页面。
- **换邮件不闪**：载入时继续显示上一封，像浏览器换页，WebKit 把旧页面留到新页面画好；超过 250 ms
  （`STAYS_SHOWN`，比如要从服务器下载正文）才藏起来。藏起来时（也包括第一封）表头和附件透明，web view 上盖一层
  与页面同色的遮罩，等页面在自己的高度上画好（`p` 报的高度够了）再一起显示，最多等 1 秒。web view 的背景设成
  页面的背景：WebKit 自己的底色是白的，画好之前会露出来。

### 图片

- **内嵌图片**：取的是 `MessageBody::inline_parts`，内嵌的规则见[正文与附件](bodies.md)，读信区不另加条件。
  打开时先显示文字。已经取到的内嵌部分在建页时嵌进页面；其余的，建页时就从 HTML 算出它们在页面里的 `cid:`
  URL（`embed_inline_images` 嵌入时顺便记下没嵌的，按 URL 解析器的写法规范化，和脚本里 `new URL(src).href`
  一致，同远程图片），取到一个就 `post_message` 填进去，不重新载入页面。没有 HTML 的邮件不取。
- **发给页面的图片**由 `catmail_frontend::html::PageImages` 管：这封邮件取到的内嵌部分、显示了的远程图片、
  页面还缺哪些、页面载入了没有（`r`）。它建页面，有图片来就排进队列，页面载入后**一次只发一张**，页面回了 `d`
  再发下一张；读信区只管把消息发给 web view。换外观重建页面时，取到的内嵌部分直接嵌进去，远程图片在 `r` 后重发。
  ⚠️ nsh 没有自定义 URL scheme，图片只能以 `data:` URL 写进一段脚本（`evaluateJavaScript`），newsletter 的图片
  一张就上 MB：GUI 主线程要转义整段字符串，网页进程按顺序执行这些脚本。一次全发出去时界面会卡，换邮件时新页面
  也要排在这些脚本后面；一次一张，换邮件时剩下的就不发了（`forget_message`）。总的载入时间还是跟图片大小成正比，
  要让 WebKit 自己读图片得等 nsh 支持 URL scheme handler（longbridge/nsh#1136）。远程图片存原始字节，发送时才编成 `data:` URL。
- **远程图片**：打开时在后台列出正文引用的远程图片（`remote_image_urls`），缓存里有的（`cached_remote_images`）
  直接显示，其余的在用户点"Show remote images"后交给 `MailClient::fetch_remote_images`（限制见
  [正文与附件](bodies.md)），按下载完成的顺序逐批填进页面（`html::data_url`），按钮转圈；一张也没拿到时提示
  原因、按钮恢复。点击只对这一封、这一次打开有效，换邮件时下载随任务取消；不记住发件人。CSP 照样只放行 `data:`。
- 页面因外观变化重新建好时（脚本报 `r`），已显示的远程图片再发一遍，已取到的内嵌图片直接嵌进新页面。

### 深色

深色主题下正文显示成深色：`recolor_html`（见[正文](bodies.md)）把邮件自己的颜色按用途搬到主题色上，自带深色
样式的原样保留；颜色用了 CSS 变量的转换不了，按原样显示成浅色。纯文本没有自己的颜色，总跟主题。页面的底色、
文字色、链接色在我们的样式里，邮件自己的样式优先。外观变了就重新建页，滚动位置不变。

## 写信窗口

写信的模型在 `catmail_frontend::composer`（`ComposeFrom::start`、From 的选项、字段解析、`Lifecycle` 决定何时保存 /
发送 / 关窗、`OpenComposers` 按草稿登记窗口），`composer/` 只有界面。每个写信窗口的视图跑在它自己的 nsh 线程上；
窗口由外壳开关（`shell::opened`、`composer_closed`）。

- **每封草稿一个窗口**：再次打开同一草稿时激活已有窗口；正在读的草稿也记下，连点两次 Edit 也只有一个窗口。
- **一开始就是草稿**：新邮件 `create_draft`、回复 `reply`、转发 `forward`，窗口编辑的总是一个已存在的草稿。
  什么都没改就关闭时删除它；没改就发送照常发出。
- **自动保存**：停顿 800 ms 就 `update_draft`。引擎对后台上传另有静默期（`draft_upload_delay`），边写边存不会
  每次都在服务器换一份副本。
- **对同一草稿的引擎调用不重叠**：发送、关闭、丢弃等正在进行的保存和附件增删结束后，再做"最后一次保存 +
  发送 / 删除 / 关窗"。
- **地址**：只存解析得了的（`parse_addresses`，与发送时同一规则），显示用 `format_addresses`。自动保存时解析
  不了的栏保留上次存下的值；发送时报错；关闭时有解析不了的栏就提示并不关窗。
- **From**：`draft_senders` 给的身份，不止一个时是下拉框（原生 `popup_menu`）；选了别的账户的身份，草稿搬到那个
  账户。收到账户的增删改（或漏了事件）时重读身份和存下的草稿，已输入的内容不动。
- **关窗**：关窗按钮、Ctrl+W、Escape 都走同一流程。GTK 的 `close-request`（macOS 的 `windowShouldClose:`）总是
  拦下，交给视图决定，视图存好后请外壳销毁窗口。
- Ctrl+Enter 发送；附件可选文件（Ctrl+Shift+A）或拖进窗口。
- **退出**前先让每个写信窗口走关闭流程（`OpenComposers::close_all`），有一个关不了（地址要改、保存失败）就不退出，
  那个窗口留下。写信窗口只管"关不关得了"，不知道退出这回事（见[后台运行 / 退出](background.md#退出)）。

## 发件箱

条目的状态决定右栏给出的选择：可用的由 `OutboxStatus::can_*` 判断，措辞、先后、要先确认的在
`catmail_frontend::outbox`，`mailbox/outbox.rs` 只画出来。右栏的 To / From / Queued 也由它给出（`outbox::facts`）：
From 是条目实际发出用的身份（`OutboxEntry::identity`），叫法和写新邮件时的 From 选项一样（在所有账户的身份里区分）；发件箱里还有条目用着的身份删不掉，
所以总找得到。排队中可以 Send now（`flush_outbox`）或 Back to drafts
（`cancel_outbox`）；被拒绝的可以改回草稿或重试；已发出但没存进 Sent 的可以重试保存或放弃；**发出后未确认
（unknown）的从不自动重发**，由用户选 Mark as sent、Send again（先确认可能重复）或 Back to drafts。

## 主题与设置

- **设置**（关窗后常驻、新邮件通知、外观、浅色 / 深色各自的主题、文字大小、打开即已读、远程图片缓存）是这个
  前端的偏好，存在配置目录的 `settings.json`（`CATMAIL_CONFIG_DIR` 可换），不进引擎的数据库。读写在
  `catmail_frontend::settings`：写入经临时文件替换，在后台按顺序进行，晚到的写不会把旧设置写回去。
- 设置和主题是进程级的（`shell/`）：每个窗口的视图各在自己的线程上读，设置对话框从主窗口改。改了就写回文件，
  所有窗口（包括写信窗口）随之更新：主题和文字大小由主线程设给每个 `AppView`，外观由各视图自己
  `set_color_scheme`。
- **文字大小**是 nsh 的 `AppView::set_font_scale`；Ctrl + / Ctrl − / Ctrl 0 改它，Ctrl+, 打开设置。
- **主题都从终端调色板推导**（`catmail_frontend::theme`）：背景、前景和 16 个 ANSI 色推导出全部颜色；内置主题
  也写成调色板。导入按内容识别 Alacritty、kitty、Ghostty、WezTerm、Windows Terminal、iTerm2、foot、Xresources、
  base16 / base24 和主题 JSON，结果写进主题目录（可手工修改，启动时读取）。主题文件的格式沿用 gpui-kit 的主题
  JSON（GPUI 版留下的，已有的主题目录照样能用），frontend 不依赖它。
- **映射到 nsh**（`theme.rs`）：nsh 的主题是 shadcn 的 token。gpui-kit 的颜色键一张表映射到 shadcn 的 token，
  再加上应用自己的（`warning`、`link`、`list-hover`、六个色相等），浅色主题给浅色外观、深色给深色，经
  `set_theme_overrides` 给每个视图。nsh 读不了的颜色值（手改坏的主题文件）跳过，那个 token 用 nsh 的默认值。
  旗标、头像、徽章、发件箱的警告都用 token，不写死颜色。
- **跟随系统外观**是 nsh 自带的（Linux 上经 XDG portal）。
- **远程图片缓存**属于引擎的数据目录，不是偏好：Settings › Reading 显示它占多少，"Clear image cache"清空。
- **Accounts**：账户列表、Add account；发件身份的默认、改名、删除、"Server saves to Sent"、Add sending address。
  确认文案在 `catmail_frontend::accounts`。

## 登录页

没有账户时自动打开、不能离开，Settings › Accounts 的 Add account 也能打开。调用
`MailClient::add_account(.., Verify::Login)`，之后由后台同步做首次同步；密码或服务器填错不会留下账户行或孤立的
密钥环条目。

- 进行中不能取消：`add_account` 一旦开始就会写入，此时离开也撤销不了。
- 字段怎么读、端口的默认值（与 CLI 相同）和提交的状态在 `catmail_frontend::login`（`AccountValues`、
  `IdentityValues`、`Submission`）；表单只检查端口格式，其余由引擎的校验负责。登录也检查 SMTP。
- 登录页和发件身份的表单共用字段；任一字段按 Enter 提交。对话框的外框、按钮和只要一个名字的对话框（文件夹的
  名字、发件身份的名字）在 `form.rs`：提交时转圈，失败留在对话框里说明原因，存好才关。

## 窗口里的通知

`toast.rs`：窗口右下角一叠，最新的离角最近，过一会儿自己消失；被拒的改动、出错（"Couldn't …"）都在这里说。
⚠️ nsh-base 只有无样式的 toast 原件，样式和计时是我们的。系统通知（新邮件）见[后台运行](background.md#系统通知)。

## 图标

[Heroicons](https://heroicons.com)（MIT），只把用到的 SVG 放在 `crates/catmail-gui/assets/heroicons/`（附许可证），
`include_str!` 编进程序，用 nsh 的 `svg` 画，`currentColor` 跟随所在处的文字色（`icons.rs`）。选中的行换成填充的
那一版。应用图标在 `crates/catmail-frontend/assets/app-icon/`：托盘用它们，打包时由它生成 `.icns`。

## 调试用的环境变量

不改设置就能截图、检查：`CATMAIL_GUI_APPEARANCE=dark|light` 强制外观，`CATMAIL_GUI_OPEN=<序号>` 打开列表里第几封，
`CATMAIL_GUI_HTML=<文件>` 用这个文件的 HTML 替换正文（检查页面能加载什么），`CATMAIL_GUI_SETTINGS` 一打开就开设置，
`CATMAIL_GUI_UNFOLD` 展开所有账户。

## 测试

纯逻辑在 `catmail-frontend`，表驱动测试，不用编译 GTK 和 WebKit（见[测试](testing.md#gui)），读信区的正文页面
也在那里测（`html`：CSP 在正文之前、`cid:` 图片嵌入、纯文本转义、消息协议的解析、页面编号、载入后补发哪些图片）。
这个 crate 只测几处不经窗口的：页面高度够不够、页面算深色还是浅色、主题映射、图标、托盘的绘制。还没有驱动窗口
的测试（ROADMAP 里的 nsh-testing）。

## 已知问题与上游

- **常驻时的内存**：GTK 常驻时仍连着显示服务器。WebKit 的网络进程原来在最后一个 web view 关掉后还留着（约 41 MB
  PSS），nsh 已改成随最后一个 web view 结束它（longbridge/nsh#1051），还要在 Linux 上确认。数字见
  [后台运行](background.md#引擎属于应用不属于窗口)。
- **拖动分隔条**：三栏的宽度由应用自己记，用 nsh-base 的 `resize_handle`。⚠️ 它的 `h_resizable` 快速来回拖时会丢掉
  拖动结束，之后分隔线错位；所以每次拖动从开始时的宽度算起，丢了结束也不会累积。
- **cargo deny 还不过**：nsh 带进来的许可证（webpki-roots 的 CDLA-Permissive-2.0，target-lexicon、winx 的
  LLVM-exception）和 nsh-updater 用的 zip 8 的 advisory（要 zip 9，longbridge/nsh#1050），见 ROADMAP。
- macOS 的 `title_bar()` 高度（longbridge/nsh#1068）、输入框失焦（#248），见上文[外壳](#外壳)。
- **图片多的邮件载入慢**：nsh 的 web view 没有自定义 URL scheme，图片只能以 `data:` URL 写进脚本发给页面，见上文
  [图片](#图片)。现在一次发一张，换邮件不卡，但总时间跟图片大小成正比。已提 longbridge/nsh#1136；支持后改成由
  WebKit 经 scheme handler 自己读图片，要改的地方见 ROADMAP。
