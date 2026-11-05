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
- [x] Drafts 是本机草稿；Outbox 只在有待发时出现，有问题时带警告图标（⚠️ nsh 主题没有 warning 色，先用
      destructive）。两者的列表随 P3 的来源一起做，现在是空的
- [x] Sent、Junk、Trash 不显示未读数
- [x] 底部一行说明连接问题（`account_status`），截断时悬停显示全文
- [x] 侧栏一次读完（`overview`）；`DraftsChanged` 只重数该账户的草稿（`count_drafts`、`DraftCounts`）。
      引擎事件按 `Refresh::after` 决定重读什么，200 ms 节流（`Throttle`），读侧栏一次一个（`Coalesce`）
- [x] 右键菜单：新建、重命名、删除文件夹和标签；F2 重命名（docs/architecture/folders.md「界面」）。
      菜单是原生的 `popup_menu`，删除确认是原生的 `alert_dialog`，命名用 `dialog()` 加输入框
- [x] 打开文件夹时同步它（`sync_folder`）
- [x] 账户可以折叠：只有一个账户时默认展开，多个时默认折叠，选中某处时展开它的账户
- [ ] 子文件夹折叠（gpui 版每个有子文件夹的行可以收起）；折叠的动画
- [ ] 底部的 Settings 和 Sync 按钮（随 P7 设置、P4 同步）

## P3 邮件列表（gui.md「邮件列表」）

行的模型在 `catmail-frontend`（`list`：来源怎么分页读、按天分组、重读后选中去哪、按标记勾选、行上的文字；
`mailbox::list_title`、`list_note`），与 gpui 版共用；nsh 版只有界面（`list.rs`）。

- [x] 按天分组，标题用 Today、Yesterday 等。⚠️ nsh 列表的 sticky header 只能在建表时给一次，分组随数据变，
      所以标题是列表里的普通行，不吸顶（gpui 版的 section 标题也不吸顶）
- [x] 两种行形：读信区关着时一行排开，打开后两行；切换时把选中的那封滚到中间。nsh 列表按行量高度，
      两行时主题按实际行数占高，不必像 gpui 版那样总占两行
- [x] 头像（姓名首字，底色按地址挑）；未读只用字重表示；Gmail 用户标签小标签（最多两个）
- [x] 悬停的行显示 To 或 Cc
- [ ] 悬停的行显示操作按钮（随 P4 的命令）
- [x] 勾选多封：点头像或按 X 勾选；标题的勾选框和按标记勾选的菜单（原生 `popup_menu`）
- [ ] 有勾选时标题换成批量工具栏（随 P4 的命令）
- [x] 搜索：200 ms 防抖；先显示本地结果，1 s 后再问服务器（`search_server`）；选侧栏时清空搜索
- [x] 刷新规则对齐 `refresh::Refresh::after`：200 ms 节流、合并请求、结果没变不替换、按行的身份保持选中；
      选中的邮件消失时清空读信区；消失的邮件不再勾选
- [x] 来源：草稿、发件箱（它们的右栏随 P5、P6）
- [x] Escape 关上读信区

## P4 读信与操作（gui.md「读与改」「读信」）

- [ ] 工具栏：Archive、Delete、Move to、Labels（Gmail）；已读 / 未读、Flag；Show remote images、Open in browser；
      Reply、Reply all（`can_reply_all`）、Forward
- [ ] 读信区工具栏、勾选工具栏、悬停的行用同一套按钮（`message_commands`、`Applies`、`MovePlan`）
- [ ] 打开即已读（设置里可关；只在用户选中时标记）
- [ ] 归档、删除、移动后打开下一封（`expect_removal`、`removal_settled`）
- [ ] 永久删除先确认（`DeleteOutcome`）
- [ ] "Move to"、"Labels"对话框（`FolderPicker`）
- [ ] 服务器拒绝的改动用通知说明（`ChangesRefused`、`dismiss_settled`）
- [ ] 远程图片：先显示缓存里的（`cached_remote_images`）；点击后用 `fetch_remote_images` 逐批显示。
      ⚠️ 不重新加载页面，而是由注入脚本经 `post_message` 把 `data:` URL 填进对应的 `<img>`，否则会丢掉滚动位置。
      CSP 照样只放行 `data:`
- [ ] 附件列表；点击后弹系统保存对话框（`pick_save_path`、`safe_filename`）
- [ ] 头部：头像；收件人一行只列名字；"Details"展开完整头部
- [ ] 在浏览器打开（`hand_off`）
- [ ] 正文的选中、复制、查找（WebKit 自带，接上快捷键）

## P5 写信窗口（gui.md「写信窗口」，依赖 P1 多窗口）

- [ ] 每封草稿一个窗口；同一草稿只开一个（`OpenComposers`）
- [ ] 一开始就是草稿（`create_draft`、`reply`、`forward`）；没改就关窗时删掉
- [ ] 停顿 800 ms 自动保存；同一草稿的引擎调用不重叠；发送、关闭、丢弃前做最后一次保存（`Finish`）
- [ ] 地址栏：解析和显示规则（`parse_addresses`、`format_addresses`），关窗前检查
- [ ] From 下拉（`draft_senders`）；身份变化时重读身份和草稿
- [ ] 附件：选文件或拖进窗口（nsh file-drop）
- [ ] Ctrl+Enter 发送；关窗按钮走同样的流程。⚠️ 要确认 nsh 和 `AppView` 能不能拦截窗口关闭
- [ ] 退出（Ctrl+Q）前先关所有写信窗口（`close_all`）

## P6 发件箱（gui.md「发件箱」）

- [ ] 右栏按条目状态给出操作：Send now、Back to drafts、重试、Mark as sent、Send again（`OutboxStatus::can_*`）
- [ ] 后台发送（`send_in_background`）；`unknown` 的条目从不自动重发

## P7 设置与主题（gui.md「主题与设置」）

- [ ] 设置对话框：外观、浅色和深色各自的主题、文字大小（`AppView::set_font_scale`）、打开即已读
- [ ] Reading：远程图片缓存的大小和"Clear image cache"
- [ ] Accounts：账户列表、Add account；发件身份的默认、改名、删除、"Server saves to Sent"、Add sending address
- [ ] 主题：终端调色板推导出 nsh 的 shadcn token（`set_theme_overrides`）；导入各种终端配色格式；主题目录
- [ ] 跟随系统外观（nsh 自带，经 XDG portal）

## P8 登录页（gui.md「登录页」）

- [ ] 没有账户时自动打开；`add_account(.., Verify::Login)`；进行中不能取消
- [ ] 表单字段与发件身份的表单共用（`LoginFields`、`ServerFields`、`submission::Submit`）

## P9 常驻与系统集成（docs/architecture/background.md）

- [ ] 托盘：ksni，显示未读数、Sync now；把 `tray/linux.rs` 里的 gpui-kit 类型剥掉后复用
- [ ] 关窗后常驻、重开窗口；Ctrl+Q 退出；`shut_down`
- [ ] 新邮件通知（`new_mail.rs`）；点击打开对应账户的 Inbox
- [ ] 常驻时回收内存（`malloc_trim`），并量一下 WebKit 子进程常驻时的占用
- [ ] 快捷键全集（对齐 `actions.rs`，用 nsh `keymap`）

## P10 平台与工程

- [ ] 字体：Pango 会自己做回退，`fonts.rs` 里那些变通预计用不上。要验证中文各字重、粗体是否正常
- [ ] 测试：用 nsh-testing 无头驱动窗口，对应 gpui 版的 `mailbox/tests/*`（读信、写信、设置、文件夹）
- [ ] 回到主 workspace：等 longbridge/nsh#966 修好
- [ ] CI：Linux job 装 GTK4 和 WebKitGTK；cargo deny 的许可证（CDLA-Permissive-2.0、LLVM-exception）和 bincode advisory
- [ ] GTK 4.22 上 `gtk_widget_measure` 警告刷屏
- [ ] 打包：`.desktop`；运行时依赖 GTK 4.14+ 和 WebKitGTK 6.0
- [ ] 与 gpui 版对比：内存（含 WebKit 子进程）、滚动帧率、冷启动时间
- [ ] macOS（WKWebView）、Windows（WebView2）后端
