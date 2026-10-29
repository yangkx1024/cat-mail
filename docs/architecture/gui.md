# GUI

[← 架构](../architecture.md)

`catmail-gui`，基于 `gpui-kit`（longbridge）：虚拟列表、Table、编辑器、原生 HTML 渲染，无 WebView。
邮件 HTML 默认由 `catmail-render` 的 Blitz 排版，见 [HTML 引擎](#html-引擎)。
gpui-kit 精确 pin 它所用的 GPUI，应用只依赖 `gpui-kit`（`use gpui_kit::*` 就是 GPUI），不直接依赖
`gpui`，否则会链接两份 GPUI。我们开启 gpui-kit 的 `gpui-fast` feature，后端从默认的 `gpui-pre-*` 快照
换成 GPUI Fast（`gpui-fast-*`，渲染和布局上的优化），导入路径不变；feature 是叠加的，`gpui-pre-*`
仍会编译，但应用只看到 GPUI Fast 的类型。gpui-kit 的编码与设计规范作为 skill 放在
`crates/catmail-gui/.claude/skills/`，改 GUI 前先读。

M5 的界面设计稿（三栏、写信窗口、发件箱、设置与主题导入、配色映射表）见 Claude Design 画布
"cat-mail M5 UI"；实现以本文为准。

## 结构

按窗口和功能分目录，一个目录一个视图；超过几百行的视图再按职责拆成子模块。

```
src/
  main.rs            启动：交给已在运行的实例或成为它，设置、主题、快捷键；主窗口（`MainWindow`）
  resident.rs        `Resident`：引擎、主窗口的关闭与重开、常驻时 headless（见[后台运行](background.md)）
  instance.rs        单实例：数据目录里的 socket，之后的启动经它唤起主窗口
  memory.rs          常驻时把窗口释放的内存还给系统（glibc 的 `malloc_trim`）
  actions.rs         Action、key context 与快捷键
  assets.rs          资源：Heroicons，及替换 gpui-kit 组件图标
  format.rs html.rs  时刻与日期分组（其余显示格式来自 `catmail_core::display`）；HTML 渲染策略
  layout.rs          共用的工具栏、右栏页面、名称与值的行、附件块、危险操作的确认框
  mailbox/           主窗口
    mod.rs           状态（账户、Place）、引擎事件、读侧栏、搜索、渲染与 Action 分派
    commands.rs      对邮件的命令（标记、归档、删除、移动）、写信、同步、被拒改动的通知
    sidebar.rs panes.rs draft.rs outbox.rs   侧栏；三栏与邮件工具栏；草稿、发件箱的右栏
    refresh.rs       哪个引擎事件重读什么、节流（纯逻辑）
    message_list/    中栏列表（mod.rs：来源、加载、选中；rows.rs：三种行的样子）
    folders.rs       文件夹分区（纯逻辑）
    folder_picker.rs add_account.rs   "Move to"与"Labels"对话框；登录页
    tests/           驱动窗口的集成测试，按读信、写信、设置分文件，另有截图
  reader/            读信：mod.rs 状态与操作，message.rs 打开的邮件，images.rs 内嵌与远程图片，view.rs 渲染，
                     page.rs Blitz 排版的页面
  composer/          写信窗口：mod.rs 打开与编辑，lifecycle.rs 保存 / 发送 / 关闭，view.rs 渲染
  settings/          mod.rs 偏好（settings.json）；dialog/ 设置对话框（外观与主题、账户、读信分文件）
  theme/             palette / derive / builtin / import/（纯逻辑：调色板、推导、内置、导入）；
                     appearance.rs 可选主题与应用；colors.rs 应用自己的颜色
```

- `Mailbox`：根视图。状态是账户列表（每个账户的文件夹、草稿数、发件箱、连接状态）和侧栏选中的位置
  `Place`（文件夹、本机草稿、发件箱）；登录页打开时整个替换三栏。对邮件的命令（回复、归档、删除、
  移动、标记……）都是它的方法（`commands.rs`），工具栏按钮、菜单和快捷键调用同一个方法或 Action。
  Reply、Reply all、Forward 在读信区正文上方那一行（与 Show remote images、Open in browser 同行），按钮
  经 `Mailbox` 的焦点句柄派发它的 Action（`FocusHandle::dispatch_action`），焦点不在邮箱里时也能到达。
- `MessageList`：gpui-kit `List` 的 delegate，一个列表显示四种来源（`Source`）：文件夹（分页）、
  搜索结果、本机草稿、发件箱。行按身份（`RowId`）保持选中。搜索框停顿 200 ms 才搜索，不是每个键都
  读一遍。
- 纯逻辑（文件夹分区、日期格式、HTML 策略、设置文件、主题推导与导入）不依赖窗口，单测覆盖。

## 侧栏

每个账户先列有角色的文件夹，顺序固定（Inbox、Drafts、Outbox、Sent、Archive、All Mail、Flagged、
Junk、Trash），名字用角色名（`FolderRole::label`）而不是服务器上的名字（`已发送` 显示为 Sent）；
图标和顺序在 `folders::RoleInfo` 一张表里；不论嵌套在哪（Dovecot 的
`INBOX.Sent`）都提到这一层，原父级没有其他内容时去掉（`folders::AccountFolders`；`\Noselect` 的父级如
Gmail 的 `[Gmail]` 也一样）。其余文件夹按服务器的层级跟在后面。

- **Gmail 的标签单列一节**"Labels"，在文件夹之后：系统标签有角色，与位置一起排在前面；用户标签按服务器
  的层级，图标是标签；在 Gmail 设置里不在 IMAP 中显示的（`listed = 0`）不列。"所有邮件"在 IMAP 中隐藏时
  （`Account::all_mail_hidden()`），账户名下面一行说明并告诉用户怎样恢复（见[位置与标签 / 界面](labels.md#界面)）。

- **Drafts 是本机草稿**，不是服务器的 Drafts 文件夹：那里是本机草稿的副本，按邮件列出会重复、也
  不能编辑。代价是其他客户端写的草稿在 GUI 里看不到（CLI 的 `catmail ls Drafts` 仍能看到）。
- **Outbox 只在有待发邮件时出现**，有被拒绝或未确认的条目时带警告图标。
- Sent、Junk、Trash 不显示未读数。底部一行说明连接问题（同步中、登录被拒、连不上、发不出去；发不出去时列出每个发不出的地址），截断时悬停显示全文。

## 字体（Linux）

GPUI 的 Linux 后端（cosmic-text）只在一个字族的多个独立字面里按字重挑选，可变字体只有一个字面，总是
按默认字重绘制；发行版现在多数只装可变的 Noto Sans 和 Noto Sans CJK，于是所有加粗都失效，CJK 还偏细。
`fonts.rs` 因此：

- **自带静态的 Noto Sans**（Regular、Medium、SemiBold、Bold，SIL OFL，`assets/fonts/noto-sans/`，
  约 1.7 MB，只在 Linux 编入），并写进每个应用的主题配置的 `font_family`：应用主题会设置它指定的字体，
  只改全局主题会在切换主题时被覆盖。
- **CJK 回退字体**：启动时从常见字族（Noto Sans CJK、思源黑体、MiSans、HarmonyOS Sans 等）里挑出已安装
  且常规和粗体是不同字面的（即非可变字体），作为 `FontFallbacks` 由一个 `RootPlugin` 设在每个窗口的根上，
  对话框、通知也在其中；回退字体按主字体的字重匹配。都不满足时 CJK 仍由 GPUI 自己挑。不自带 CJK 字体：
  每个字重十几 MB。

macOS、Windows 用系统字体，不受影响。

## 邮件列表

- **按天分组**：列表（邮件、搜索结果、草稿、Outbox 都是新的在前）按本地日期分成 gpui-kit `List` 的
  section，标题是 Today、Yesterday、今年的"Monday, October 12"、更早的"October 12, 2025"，没有日期的
  归到 No date；行里只显示时间。分组只合并相邻的同一天，顺序不是按日期时一天会出现两次标题，而不会
  错位。`List` 按 `IndexPath { section, row }` 计数，`rows` 和选中项仍按从上到下的序号，两者由
  `message_list::Days` 换算。标题在加载、刷新时重算：窗口跨过午夜后，下一次刷新才把"Today"改成
  "Yesterday"。
- **未读只靠字重**：未读邮件的发件人和主题加粗，不再有左侧的圆点。
- **Gmail 的用户标签**是主题后面的中性小标签，最多两个、其余计数，不显示正在看的那个标签。名字由
  `Mailbox` 从侧栏的账户里取（`MessageList::set_labels`），读信区的标签也由它按列表里这封邮件的现状设置
  （观察列表，`Reader::set_labels`）：标签在邮件打开时也会变。

## 读与改

- **打开即已读**（设置可关）：列表选中一封未读邮件就 `set_flags(SEEN)`，经操作队列到服务器。只在用户
  选中时标记，刷新后恢复选中不算。
- **归档 / 删除 / 移动后打开下一封**：命令先让列表记下要移走的邮件（`expect_removal(id)`），刷新后
  它仍是选中的那封却不在了，就选中同一位置的那一行。邮件不一定离开：Gmail 上在"所有邮件"里移到某个标签、
  在 Work 里归档，它还在列表里；搜索结果不论邮件在哪都列出。会不会离开只由存储列出邮件的规则决定，前端
  不预测：引擎调用返回时改动已经提交，`message_list::removal_settled` 让列表再读一次，这次（及之后开始的）
  读取里邮件还在，就不再等它、选中不变；调用失败也不再等。之前已在途的读取可能早于改动，不算数。每次
  `expect_removal` 有自己的编号，只有最近一次的回来才算：先归档再删除同一封时，归档那次再读到它，不会让
  删除不再等。服务器那边消失的邮件仍然只清空读信区。标记按邮件
  身份认：用户换了选中，标记即作废，之后别的邮件消失不会被当成"移走后打开下一封"。
- 已在 Trash（或账户没有 Trash）时删除是永久的，先用 AlertDialog 确认（`layout::confirm_danger`）；
  其余可逆的操作不确认。是否永久由 `DeleteOutcome` 判断，能否归档、移到哪些文件夹由 `MovePlan` 判断（移动带上
  正在看的文件夹，Gmail 上从这个标签移走；归档要真的改变这封邮件，`MovePlan::changes`：Gmail 上不在 INBOX 的
  不能归档），都基于
  账户的文件夹列表而不是侧栏的顺序：有两个 Trash 时，侧栏先列的不一定是引擎移入的那个，而在任一
  Trash 里删除都是永久的。
- **标签**（Gmail）：工具栏的标签按钮或 L 键打开"Labels"对话框（与"Move to"同一个 `FolderPicker`，
  带勾选），点一个立即加上或去掉，对话框留着；普通账户没有这个按钮。勾选由 `Mailbox` 控制：点击只报告点了
  哪个标签，加还是去按邮件现在的标签定；列表每次变化都按这封邮件现在的标签重设勾选（被拒绝后撤销的、同步
  拉到的也一样），这封邮件不再是选中的那封（例如在标签里去掉这个标签，列表打开了下一封）就关掉对话框
  （`sync_labels_dialog`），不会去改另一封。
- 服务器拒绝的改动：`ChangesRefused` 后 `dismiss_settled` 在一个写事务里取出并忘掉所有失败或冲突的
  改动，逐条以通知说明，不会重复提示。引擎只在改动变成失败或冲突时发这个事件，本地改动不触发查询；
  窗口打开时和漏收事件之后也查一次（之前留下的、窗口没开时被拒绝的）。它先读一下有没有这样的改动，没有
  就不占写连接（同步时写连接常被占着）。

## 写信窗口

每封信一个窗口（`OpenComposers` 全局按草稿记录窗口，再次打开同一草稿时激活已有窗口；读草稿是异步的，
正在读的草稿也记下，再次打开直接返回，连点两次 Edit 也只有一个窗口；建窗口前再查一次，回复 / 转发的
窗口还没出来时从草稿列表打开它的草稿，也只有一个窗口。读草稿的任务挂在应用上而不是发起的窗口上：那个
窗口先关了也不会让草稿一直处于"正在打开"）。

- **一开始就是草稿**：新邮件 `create_draft`、回复 `reply`、转发 `forward`，窗口编辑的总是一个已存在的
  草稿，不需要"第一次保存时创建"的分支，也不会因并发保存建出两份。窗口里什么都没改就**关闭**时删除它；
  没改就**发送**照常发出（不用加字的回复）。
- **自动保存**：每次输入后 800 ms 无输入就 `update_draft`。引擎对后台上传另有 15 秒的静默期
  （`draft_upload_delay`），所以边写边存不会每次都在服务器换一份副本。
- **对同一草稿的引擎调用不重叠**：同一时间最多一次保存；发送、关闭、丢弃（`Finish`）等正在进行的
  保存**和附件增删**结束后再做"最后一次保存 + 发送 / 删除 / 关窗"：较早的保存不会在发出的内容之后
  落地，刚拖进来的附件也不会因为草稿已在发送而漏掉。附件加不上（或删不掉）时，等着的发送或关闭取消，
  窗口留下并显示原因。
- **地址**：草稿只能存解析得了的地址（`parse_addresses`，与发送时同一规则）。栏里显示的地址用
  `format_addresses`，含逗号、引号等的名字加引号，读回来不变（回复 `Doe, Jane` 不用手改）。自动保存时
  解析不了的栏保留上次存下的值；发送时报错；关闭时只要有解析不了的栏就提示并不关窗（存下的总能解析，
  所以解析不了就是没存下的输入），自动保存之后也一样。关不关看各栏内容与存下的是否相同，不看"改过"
  标记：最后几个键的事件可能还没处理。
- **From**：账户有不止一个身份时是一个下拉框（默认的在最前），选了就像其他栏一样自动保存；同一个
  地址出现两次（走两台服务器）时后面附上服务器。只有一个时只显示地址。窗口打开时读一次身份列表，之后
  自己订阅引擎事件，收到本账户的 `AccountChanged`（或漏了事件）就重读：选着的身份还在就留着，被删了就
  换成默认身份，已输入的内容不动。在那之前的保存也不会失败：存草稿时传来的身份已被删除，引擎就保留草稿
  现在的身份（删除时已改成默认身份；身份 id 不重用）。
- 窗口边框的关闭按钮经 `on_window_should_close` 走同一流程（需要先保存时返回 false，存完自己关）。
- Ctrl Enter 绑在 `Composer > Input` 上直接是 `Send`：多行输入框自己处理时会先插一个换行，再随信发出。
  附件可选文件或拖进窗口（`ExternalPaths`）。
- **退出**（Ctrl Q）由 `Resident::quit` 决定：先让每个写信窗口走关闭流程（`composer::close_all`，存下
  最后的输入），全部关掉后才退出；GPUI 的 `quit` 不经过窗口的关闭回调。有窗口关不了（地址要改、保存失败）
  时 `close_all` 立即答"没关完"，那个窗口留下，退出取消。写信窗口只管"关不关得了"，不知道退出这回事。

## 发件箱

客户端开启 `send_in_background`。条目的状态决定右栏给出的选择（哪些可用由 `OutboxStatus::can_*`
判断，按钮的措辞和先后在 `outbox.rs`）：排队中可以"Send now"
（`flush_outbox`）或"Back to drafts"（`cancel_outbox`）；被拒绝的可以改回草稿或重试；已发出但
没存进 Sent 的可以重试保存或放弃；**发出后未确认（unknown）的从不自动重发**，由用户选"Mark as sent"、
"Send again"（确认可能重复）或"Back to drafts"。

## 主题与设置

- **设置**（外观、浅色 / 深色各自的主题、文字大小、打开即已读）是这个前端的偏好，存在配置目录的
  `settings.json`（`--config-dir` / `CATMAIL_CONFIG_DIR`），不进引擎的数据库。写入经临时文件替换，
  在后台按顺序进行：每次写持锁取最新的设置，晚到的写不会把旧设置写回去。读不了就用默认值。
- **远程图片缓存**属于引擎的数据目录，不是偏好：Settings › Reading 显示它占多少（每次切到这一节时重新
  量，缓存随阅读增长），"Clear image cache"清空；缓存为空时按钮不可用，清空中转圈。已经显示在读信
  区的图片留在内存里，换一封再回来才重新拦截。
- **主题都从终端调色板推导**（`theme/`）：背景、前景和 16 个 ANSI 色按设计稿的映射表推导出 gpui-kit
  `ThemeConfig` 的全部颜色键；内置主题也写成调色板，与导入的走同一套规则。导入按内容识别 Alacritty、
  kitty、Ghostty、WezTerm、Windows Terminal、iTerm2、foot、Xresources、base16 / base24 和 gpui-kit
  主题 JSON，推导结果写成 gpui-kit 主题 JSON 放进主题目录（可手工修改，启动时读取）。新主题的文件
  不与别的主题共用（名字不同、文件名相同时加序号）；重新导入的主题写回原来的文件，同一文件里的其他主题
  保留。
- **不用 gpui-kit 的 `ThemeRegistry`**：它不能替换同名主题（重新导入要覆盖旧的），也会自己监视目录、
  改全局主题。可选主题由 `appearance::Themes` 全局保存，应用时把选中的配置放进 `Theme::light_theme` /
  `dark_theme` 再 `Theme::change`。
- **设置改了就生效**：`appearance::init` 观察 `Preferences` 全局，任何地方 `Preferences::update` 之后
  主题和文字大小随之应用，调用方不必各自记得。
- **跟随系统**：主窗口 `observe_window_appearance`，把窗口看到的外观记在 `Themes` 里再应用（Linux 上
  应用级的不可靠）。
- **文字大小**就是 `Theme::font_size`，也就是窗口的 rem，间距、图标随之缩放。
- 应用自己需要而 gpui-kit 没有角色的颜色（旗标、徽章、头像底色）由 `theme/colors.rs` 从主题已有的
  颜色混出，不写死颜色值；主题卡片和导入预览里的色块是数据（主题本身的颜色），例外。

## 运行时与事件

`MailClient::builder(cfg).open_async()` 自建 tokio runtime：主窗口先打开、留空，数据目录打开（可能
要跑一次迁移）后再换成邮箱，打不开时显示原因。客户端由应用级的 `Resident` 持有，不属于主窗口：关掉
主窗口后应用默认继续运行，再打开时新建邮箱（见[后台运行](background.md)）。视图只在 `cx.spawn` / `spawn_in` 中 await 门面
方法，从不碰 tokio。事件的 `broadcast::Receiver` 不依赖 reactor，直接在 GPUI 的 executor 上 `recv`；
`Lagged` 时整体刷新（侧栏、列表，并查一次被拒绝的改动），`Closed` 时退出。

客户端开启了后台同步（`sync_in_background`）：服务器推送的新邮件同样经 `MessagesChanged` 出现。GUI
自己不同步账户：启动、添加账户时后台同步本来就做一次完整同步；Sync 按钮、Ctrl R 和托盘的"Sync now"
调 `MailClient::sync_now`，由后台同步来做（见[同步 / 后台同步](sync.md#后台同步与-idle)）。进度和失败都
来自账户状态：有账户 `syncing` 时侧栏底部显示"Syncing…"、Sync 按钮转圈，失败时显示连接问题；
不再弹同步失败的通知。后台同步在两次完整同步之间只盯
INBOX，所以打开一个文件夹时同步它（`MailClient::sync_folder`，一分钟内同步过的跳过，见
[同步 / 打开的文件夹](sync.md#打开的文件夹)）；失败只记日志，连不上服务器已由侧栏底部说明。

哪个事件重读什么由 `refresh::Refresh::after` 一张表决定：

- **侧栏**一次读完：`MailClient::overview` 在一个读事务里取所有账户的文件夹（含计数）、草稿数和发件箱，
  不是每个账户 2 次查询、各自一次运行时往返。连接状态不在数据库里，不进快照：结果落到窗口时才取
  `account_status`，所以不会比它的事件旧。账户增删、`FoldersChanged`、`MessagesChanged`（计数）、
  `OutboxChanged` 重读侧栏。
- **草稿数**：写信窗口每次停顿都保存，`DraftsChanged` 只重数该账户的草稿（`count_drafts`）。它和侧栏的
  读取互相没有先后，所以按账户记下收到的 `DraftsChanged` 次数（`refresh::DraftCounts`）：开始得比最近一次
  早的计数或侧栏读取，不改这个账户的草稿数，由那次事件引起的计数来改。接收端 lag 时先给每个计数加一、
  再重读侧栏：丢掉的事件里可能有 `DraftsChanged`，在途的旧计数作废，侧栏读到的草稿数照常写入。最近
  一次计数失败时改为重读侧栏，否则草稿数会停在旧值直到下一次 `DraftsChanged`。
- **列表**：`MessagesChanged` 且列表显示这个文件夹，或显示搜索结果（搜索跨所有账户和文件夹，任何
  文件夹变了都可能影响结果；账户增删、`FoldersChanged` 也重读搜索结果，里面可能有已经不在的账户或
  文件夹的邮件）；`DraftsChanged` / `OutboxChanged` 且显示该账户的草稿 / 发件箱。重读已
  显示的行，按行的身份保持选中（`message_list::reselect`）。选中的邮件刷新后不见了（被删除、
  UIDVALIDITY 改变）时，列表发出 `ListEvent::Cancel`（与按 Escape 相同），读信区随之清空；用户自己
  移走的除外（见"读与改"）。
- `AccountStatusChanged`：直接更新侧栏底部的连接说明。`ChangesRefused`：报告被拒绝的改动，同一时间
  只查一次，期间再有事件补查一次。`PendingOpsChanged` 什么也不触发。
- **节流**（`refresh::Throttle`）：一次读取之后 200 ms 内的事件先攒着，间隔结束时合在一起读一次，然后
  再等一个间隔；一个间隔里没有事件就回到空闲，下一个事件立即读。单个改动（标记已读）马上显示，一次
  同步的几十个事件每 200 ms 最多读一轮。不按"同步进行中"去抖：后台同步不经过窗口的 `syncing` 计数。
- 节流之下，读侧栏和刷新列表各自还合并请求（侧栏和被拒绝的改动用 `refresh::Coalesce`）：同一时间最多一次读取，期间再有事件只记一个"过期"标记，
  完成后补读一次。只替换 GPUI 侧的任务不够，已交给引擎的读取照样会做完。切换文件夹则直接替换任务，
  带代际号丢弃过期结果。
- 读回来的和窗口正显示的一样（侧栏的账户列表、列表的行和"还有更多"）时不替换、不 `notify`：同步
  每批邮件发一个事件，多数轮次侧栏和列表都没变，比较一遍比重绘整个窗口便宜。
- **分页**：文件夹和搜索结果每页 200 条，滚到底再取下一页，不再在 500 条处静默截断。下一页按游标取
  （最后一行的 `MessageCursor`，见[存储](storage.md#列表顺序与分页)），排在它之后的行：上一页之后顶上
  新来或删掉的行不会让下一页重复或漏行，追加时不用去重。顶上的变化本身由带来变化的事件随后的刷新
  （从头重读已显示的行数）显示出来。

## 读信

- **正文。** 有 HTML 部分就由 Blitz 排成页面（见 [HTML 引擎](#html-引擎)），即使也有纯文本部分；
  只有纯文本时用 `SelectableText`。不提供 Text / HTML、引擎的切换：GPUI Kit 的 `TextView` 排不了多数
  营销邮件；Blitz 也排不好的邮件用"在浏览器打开"。打开时把正文、标题、
  头部和附件列表一次准备好（`OpenMessage`），纯文本作为同一个 `SharedString` 每帧交给视图。
- **图片。** 页面的 resolver（`Images::bytes`）决定每张图：
  - `cid:` 按 `content_id_from_url`（与引擎判断"内嵌"同一条 RFC 2392 规则）查已加载的内嵌部分；
  - `data:` 用 `data-url`（gpui-kit 自己用的同一个 WHATWG 解析器）解码；
  - 远程（`http`、`https`）图片在用户点了"Show remote images"之后，查这封信已下载的远程图片；
  - 其他（未下载的远程图片、`file:`、相对路径）答 `None`，不发任何请求；
  - **SVG 一律不显示**：usvg 默认会把嵌套的 `<image href>` 当本地路径直接读文件，绕过 resolver，邮件
    就能显示本地图片，或读 `/dev/zero` 耗尽内存；Blitz 也不开 `svg` feature。
  - 显示与否只有一条规则（`html::Carried::of`：内嵌部分或 `data:` 图片；再加用户要了的远程图片，
    `Images::bytes`）。`data:` 图片每次问都重新解码，不缓存：文档只在打开时问一次。内嵌图片在正文显示
    后再逐个取；没有 HTML 的邮件不取。
- **远程图片。** 打开邮件时在后台线程用 `catmail_core::html::remote_image_urls` 列出正文引用的远程
  图片（见[正文与附件](bodies.md)），`OpenMessage::remote_images` 记着其中还没显示的，不为空就在格式栏
  显示"Show remote images"按钮。打开邮件时与内嵌图片并行地查缓存（`cached_remote_images`），缓存里有的
  直接显示，不必点击，也不发请求；都在缓存里就不显示按钮。点击后把其余 URL 交给 `MailClient::fetch_remote_images`（限制见
  [正文与附件](bodies.md)），按钮转圈；图片按下载完成的顺序逐批显示：每到一张，连同此刻已到的
  （`next_ready`）合成一批，换一份 `Images`（保留已到的内嵌和远程图片，内嵌图片后到时同样保留远程的），
  交给页面（`PageView::set_images`，不重开文档，见[HTML 引擎](#html-引擎)）。新的 `Images`
  （`with_inline` / `with_remote`）保留已到的图片，内容是共享的 `Bytes`，不复制。全部结束后按钮消失
  （失败的留白）；一张也没拿到时提示第一个错误、按钮恢复，可以再试。
  - 点击只对这一封、这一次打开有效：换邮件时任务被丢弃，流随之丢弃，引擎里还在下载的立即中止；
    不记住发件人。下载过的图片进引擎的缓存（见[正文与附件](bodies.md)），下次打开直接显示。
  - URL 按 `remote_image_url`（`url` 解析后的写法）存取：Blitz 先解析 URL 再问（主机名小写、空格
    转义），原样的写法也按同一种写法查。
  - "在浏览器打开"仍按 CSP 拦截远程图片，不带已下载的远程图片。
- **链接。** 只把 `http` / `https` / `mailto` 交给系统打开（`is_openable_link`）；其他链接点了无反应。
- **在浏览器打开。** 把清洗后的 HTML 写成独立页面：`catmail_core::html::embed_inline_images` 把内嵌
  图片换成 `data:` URL（浏览器不认 `cid:`；用读信窗口已加载的图片），加 CSP
  `default-src 'none'; img-src data:; style-src 'unsafe-inline'` 和 `no-referrer`。
  - 替换在 proto 里随清洗的同一次解析完成，`data:` URL 也由 proto 构造（MIME 类型只留合法字符）：
    ammonia 不再检查属性过滤器返回的值，所以不接受调用方给的任意字符串。
  - 页面经 `MailClient::hand_off` 写进数据目录的 `handoff/` 并保留，因为浏览器在函数返回后才读它（清理
    规则见[总体架构](../architecture.md)）。不放系统临时目录：那里没人清理，整封邮件会一直留在磁盘上。
- **附件。** 列出非内嵌部分，点击后弹系统保存对话框（文件名经 `safe_filename`），需要时先下载。
- **头部。** 发件人头像是姓名首字，底色按地址从主题的六个色相里挑（同一个人颜色固定）；收件人一行只
  列名字，"Details"展开完整的 From / To / Cc / Reply-To / Date。标题、头部和正文在同一个滚动区里，
  正文限宽 48rem 便于阅读。

## 登录页

没有账户时自动打开（不能取消），Settings › Accounts 的"Add account"也能打开。调用 `MailClient::add_account(.., Verify::Login)`，
之后由后台同步做首次同步（见[同步 / 后台同步](sync.md#后台同步与-idle)）；密码或服务器
填错不会留下账户行或孤立的密钥环条目。

- 登录进行中"Cancel"不可用：`add_account` 一旦开始就会写入（引擎的阻塞任务不随窗口的任务取消），
  此时离开也撤销不了，账户照样加上、同步。
- 空的端口和用户名取与 CLI 相同的默认值；表单只检查端口格式，其余由引擎的校验负责。登录也检查
  SMTP，所以 SMTP 填错同样当场报出。邮箱、密码、名字、用户名和服务器字段在 `login_form.rs`（`LoginFields`、
  `ServerFields`），与发件身份的表单共用。
- **发件身份**在 Settings › Accounts 里，列在各自账户下面（包括随账户建出的那个），默认的标着"Default"，
  其余可以"Make default"或删除；每个身份有一个"Server saves to Sent"开关，显示的是现在的判断（用户
  没说过时是猜的），拨动即存下用户的选择，之后不再猜（见[发信 / 保存到 Sent](sending.md#保存到-sent)）；"Add sending address"在设置上面再叠一个对话框放身份的表单（地址、
  密码、名字、用户名和 SMTP 服务器，字段与登录页共用），`add_identity(.., Verify::Login)`，成功后表单
  自己关掉这个对话框。Enter、Esc、Cancel 都按对话框的约定：任一字段按 Enter 冒泡成对话框的
  `Confirm`，`on_ok` 提交并返回 false；Esc 和 Cancel 是 `Cancel`，只关最上层的这个表单，进行中
  `on_cancel` 拒绝关闭。底部两个按钮各自派发 `Cancel` / `Confirm`，点击处理放在按钮本身，获得焦点时
  空格和 Enter 才能触发它们；对话框把 Enter 绑成 `Confirm`，会在按键按下时抢先，所以按钮带
  `DialogButton` 键上下文，在那里把 Enter 绑成 `NoAction`，交还给按钮。确认框（`layout::confirm_danger`）
同理不用 `AlertDialog` 默认生成的按钮，自己给 footer，Cancel 和确认按钮带同样的上下文。表单要等对话框打开之后才把焦点
  放进 Email 字段：对话框记下打开时的焦点，关闭时还给它，先聚焦就会还给一个随表单消失的字段，
  再按 Esc 也关不掉设置。登录页不是对话框，任一字段按 Enter 提交靠 `login_form::submit_on_enter`。
- 两个表单的提交流程也共用 `login_form::Submit`：进行中不重复提交、字段校验失败或登录失败显示在按钮
  上方，以及 Cancel（进行中不可用）和主按钮（loading）的样式；登录页把两个按钮排成一行，身份表单放进
  `DialogFooter`。表单只提供字段和要调用的 `MailClient` 方法。

## HTML 引擎

gpui-kit 的 `TextView` 只排 HTML 的一个子集（行内 style 基本不读），营销邮件只能靠展开布局表格勉强
可读。所以 HTML 交给 [Blitz](https://github.com/DioxusLabs/blitz)（`catmail-render`）：Stylo（Firefox 的
CSS 引擎）+ Taffy + Parley 排版，vello_cpu 绘制。起初保留 GPUI Kit 作为可切换的备选（连同为它展开
布局表格的 `unwrap_layout_tables`），页面能选择文字之后删掉了。不开 blitz-dom 的 `svg` feature：usvg 会按本地路径读嵌套图片（见上文
"图片"）。选型经过见[下文](#选型记录)。

- **`catmail-render`** 不依赖 GPUI 和邮件类型。每个打开的文档（`Document`）一个线程：Blitz 的文档
  不是 `Send`（持有不带 `Send` 约束的 provider），只能在打开它的线程上活。请求按顺序回答：某宽度下
  的高度、一条像素（分块）、某点下的链接；调用方不再等的请求（任务被替换）直接跳过，所以拖动窗口
  不会堆积工作。丢掉 `Document`，线程答完手头的请求就退出；引擎 panic（Blitz 还在 beta）只让这一个
  文档的请求失败（`Stopped`）。请求还有：某点下有什么（链接、是否在文字上）、两处之间的文字。引擎的接口是 `worker::Page` 和一个打开函数，换引擎只换实现。系统字体
  全进程只扫描一次（`FontContext` 的克隆共享扫描结果），同一页面的分块复用光栅化器。
- **后到的图片。** resolver 暂时答不上的图片（内嵌部分还在下载、远程图片还没取），`NetProvider` 把
  Blitz 给的 `NetHandler` 按 URL 留着，而不是丢掉。`Document::set_images` 换一个新 resolver（应当答得
  出旧的答过的一切），线程按顺序收到后用它再问一遍所有还在等的 URL，答上的交给各自的 handler（在
  渲染线程上解码），下一次排版时 Blitz 收下它们，只重算受影响的样式和布局，不重新解析整页。URL 是
  Blitz 解析后的写法，读信区按 `remote_image_url` 存远程图片，两边一致。
- **超时。** 邮件可以故意做得很难排版，线程又停不下来：`PageView` 对每个请求用 GPUI 的
  `with_timeout` 等 10 秒，超时就放弃这个文档并说明原因；它的线程做完手头的工作后自行退出，别的文档
  各有线程，不受影响。页面高度最多 200 000 CSS px，再往下截掉。
- **不联网。** 引擎能拿到的只有 HTML 和读信区给的 `Resolver`，Blitz 唯一的 `NetProvider` 只问它。
  `Images::bytes` 只答内嵌部分、`data:` 栅格图片、用户要了并已下载的远程图片，样式表、字体一概拿不到。`Resolver` 返回 `bytes::Bytes`（Blitz 存图片用的
  类型）：`Images` 存的就是 `Bytes`，文档重开（主题或缩放变了）、图片再交给文档时都共用同一份内容，
  不再复制。
- **排版。** 宽度取读信区正文宽度（取整，避免亚像素变化反复重排）；视口高度固定 800 CSS px（`vh` 和
  `height:100%` 以它为准），页面整体由读信区滚动，不随窗口高度重排。Blitz 按视口的缩放系数排字，
  所以文档按打开时的缩放系数排版和绘制，缩放系数变了就重新打开。默认字体 sans-serif（用户代理样式
  表），与界面一致；邮件自己的样式优先。
- **文字大小。** 页面像浏览器的页面缩放那样随窗口的 rem 缩放：缩放比是 rem / 16（浏览器的默认字号），
  一个 CSS px 显示成这么多窗口像素，按 `窗口缩放系数 × 缩放比` 打开文档、按 `宽度 / 缩放比` 排版。
  整页缩放而不是只改默认字号，因为多数邮件自己用 px 定字号，只改默认字号几乎没有效果，也破坏了
  文字、间距一起缩放的那条轴。文字大小变了就重新打开文档。
- **显示（`PageView`）。** 页面按 512 CSS px 一条绘制，转成 BGRA 交给 GPUI 的 `img`。绘制时测量
  自身：宽度变了才重排，可见区域决定要哪几条（多画上下各一条），远离屏幕三条以外的丢掉。
  - **换页不闪。** 新的排版（换宽度、缩放系数或外观变了而重开、图片到了）回来后先不显示：旧的排版和
    它的分块继续显示，新排版（`Next`）只画屏幕上的那几条，全部到齐后在同一个回调里换掉旧的分块和
    排版，所以任何一帧都只显示一个排版的完整分块，不会空白，也不会混着两种高度（分块的高度取决于页面
    高度，按旧高度画出的末条会被拉伸）。还没画好时又来一个新排版，就直接替换它（它的分块从没画到
    屏幕上，不用 `drop_image`）。屏幕上没有分块要画的新排版（页面空了，比如媒体查询在更宽时隐藏了
    正文；或者滚动位置已在新页面之外）等不到分块，测量时发现就在这一帧画完后直接换上（`defer_in`）。
  - **图片接连到达。** `set_images` 只把新 resolver 记在 `Opened::images`，重新排版时才交给文档，紧接着
    请求排版（线程按顺序处理，排版一定用上它）。重新排版要等上一个排版已经显示、没有 `Next` 在画，
    所以连续到达的图片不会让页面一直换不过去，期间到的合并进下一次排版。不能一到就交给文档：线程会
    作废缓存的排版，之后为 `Next` 画的分块按新图片重排，内容与 `Next` 的高度对不上。
  - GPUI 把图片留在窗口的 sprite atlas 里直到 `drop_image`，所以淘汰、重排和视图释放时都要调用；
    淘汰和换页不能在绘制一帧的过程中做（那一帧已建好的 `img` 会把它放回去），放在分块到达的回调里。
  页面画在 `Appearance::canvas` 上（浅色时是白色，邮件按白底写）。
- **深色。** 深色主题下正文一律显示成深色：`prefers-color-scheme` 回答 dark，画布是窗口背景色，
  再加一张用户代理样式表给 html 的文字、背景和链接配主题色（在邮件自己的样式之下；链接用
  `a[href]`，否则 Blitz 自带样式表里同一选择器的 `#0000ee` 会赢）。自带深色样式的正文
  （`color_usage`：`Adaptive`）用它自己的；为白底设了颜色的（`Light`）像 Gmail 那样转换颜色：
  `Appearance::recolor` 在文档线程上、Blitz 解析之前对 HTML 跑 `recolor_html`（见
  [正文](bodies.md)），把样式表、`style`、`bgcolor` / `color` 里的颜色按用途搬到主题色上。图片不变。
  `PageView` 每帧比较主题的这几种颜色，变了就按新外观重新打开文档。
- **链接。** 悬停时向引擎问指针下有什么（命中测试后沿父节点找 `<a href>`，以及是否在文字上），链接
  显示手形光标、文字显示 I 形光标；点击同样先问，再按 `is_openable_link` 交给系统。拖选结束在链接上
  时页面上有选区，这次点击不打开链接。
- **选择文字。** 页面作为一个参与者加入 gpui-base 的窗口级选区（`TextSelectionHandle`，与
  `SelectableText` 同一套），所以拖选、点别处清除、Ctrl+C（Root 的 `Copy`）、跨参与者的
  选区都按窗口的规则走。窗口记下拖动两端在页面上的位置（`TextSelectionSnapshot`；选区从页面外开始或
  结束时按 coverage 取页首或页尾），`PageView` 换算成 CSS px 问引擎 `Document::select`（`Span`：两端之间，或某点周围的词、段落），
  引擎答出高亮矩形和文字；复制时同步回调直接给最近一次答到的文字（渲染线程可能正忙，不能在 UI 线程上
  等它）。拖动越过页面时页面上的两端不变（只有另一端在动），这时不再问引擎。
  - **找位置靠几何，不靠 Blitz 的命中测试。** 页面的文字在各个 inline root 里（每个是一个 Parley
    段落），按布局树（`layout_children`，含匿名块）先序遍历即阅读顺序。点落在哪个段落的内容框里就在
    那段里用 `Cursor::from_point` 找偏移；不在任何段落里时取同一行最近的段落，再不行取上方最近一段的
    末尾。Blitz 的 `find_text_position` 在段落之间返回空，拖过页边距或段间空白时选区会跳。段落列表按
    排版缓存，重排后作废。
  - **行内块按它在行里的位置读。** inline-block 自己是一个段落，排在所在段落的某个位置（Parley 的
    inline box，`index` 是它在文字里的字节位置，`id` 是节点）。所在段落的文字在这些位置切成几段
    （`Piece`），行内块的文字读在两段之间，不换行；否则全选得到 "before  after\nINNER"，拖过整行
    根本拿不到 INNER。选区的位置是（片段，段落内偏移）。恰在行内块处，偏移本身分不出块前块后，按点的几何定：
    在块所在行的下方、或同一行上越过块的中线，就算块后（Parley 排好的 inline box 位置）；行尾、行首的
    块之后留一个空片段，所以拖过整行能选上块。双击选词不带边上的块，三击选段落带上。
    高亮也要分开：Parley 的选区几何把 inline box 的宽度算进它后面那个字，整行选中时也覆盖整行，所以
    从块后开始的选区会盖住块，全选时块被画两遍（它自己的文字一遍、所在行一遍，半透明叠深）。有文字的
    块（遍历时在所在段落前后两片之间夹了别的片段）由它自己的文字高亮，所在段落的矩形在这些块的位置
    挖掉；没有文字的块（行内图片）照 Parley 的画法。
  - **不画的不选。** 与 Blitz 绘制的规则一致：元素的 `visibility` 不是 visible，或 `opacity` 为 0，
    Blitz 不画它和它里面的一切（里面再设回 visible 也不画），遍历时整棵子树跳过。邮件里隐藏的预览文字
    不会被复制、高亮在空白处。
  - **高亮由 GPUI 画，不进分块。** Blitz 自己的选区（`set_text_selection`）会以固定的浅蓝色画进页面，
    深色时文字看不清，而且每动一下就得重画屏幕上的分块（vello_cpu 每条几十毫秒）。所以不用它，
    引擎只答矩形，`PageView` 在分块之上用主题的 `selection` 色（半透明）画可见的那些。分块不透明，
    高亮只能盖在文字上面，`SelectableText` 是画在文字下面。
  - **重排后重新问。** 选区存的是窗口坐标，与 GPUI Kit 的文字一样按几何定位；换宽度、图片到了、
    重开文档后，新排版显示出来再问一次引擎，旧排版的矩形不画在新排版上。排版按序号区分
    （`Layout::number`），不按尺寸：图片落在固定高度的盒子里时，页面尺寸不变、文字却移动了。新排版
    还没显示（`Next` 在画，或高度未答）时不问：引擎会按新排版回答，却被当成显示中的那个。
  - **双击选词、三击选段落**：窗口的选区要靠参与者的文字 run 找词，页面没有，所以页面自己处理：
    按下第二、三次时问引擎 `select_around`（Parley 的 `word_from_point`、`hard_line_from_point`，
    段落以 `<br>` 或块为界），作为参与者的本地选区（`set_local_selection`）参与复制。窗口在 capture
    阶段清空选区的通知晚于这次按下才到，所以本地选区还在时忽略清空通知。
  - 选区开始时页面获得焦点（`focus_with`），按键才能经过 Root 复制。
  - **全选**：`SelectAll`（gpui-base 的 action），页面获得焦点时 Ctrl+A（`READER_PAGE` 键上下文）或
    右键菜单触发。先清掉窗口选区，再以页首到页尾作为页面的本地选区。
  - **右键菜单**：页面上右键弹出 "Copy" 和 "Select all"（`context_menu`），没有选中文字时 "Copy"
    置灰。"Copy" 不能像按键那样派发 `Copy` 交给 Root：选区层在 capture 阶段对任何左键按下都清空窗口
    选区，包括按在菜单上，等菜单项执行时已经没有选区了。所以菜单打开时记下窗口选中的文字（`TextSelection::selected_text`，与 Ctrl+C 一致），菜单项直接
    写剪贴板；复制后选区随这次按下消失，这是正常的交互，不放回去。"Copy" 挂着 `Copy` action 只为显示
    快捷键（有 handler 时不派发 action）；"Select all" 直接派发 `SelectAll`，按下菜单清掉的选区与它无关。
  - **菜单之后的第一次按下**：菜单项的点击会 `stop_propagation`，选区层收不到这次松开，它的
    "按下已处理" 标记（`mouse_down_prepared`）要到下一次松开才复位，期间的那次按下不清空选区。窗口
    选区照样被新的拖动替换，但参与者的本地选区（全选、双击）会留下。所以页面在窗口上注册一个 capture
    阶段的左键按下监听，按在任何地方时若页面有本地选区就调 `TextSelection::clear`。这是 gpui-kit 0.8.0 的问题，升级时可以再看。
  - 段落之间用换行连接；表格单元格也各占一行（浏览器用制表符）。
- **还不支持**：页内查找、单封邮件切回原色的开关（暂不需要：Gmail 也没有）。

### 选型记录

2026-10-22 评估，按顺序：

1. **嵌入 WebView（wry / gpui-webview）。** GPUI Fast 有原生视图合成，gpui-webview 已接好 macOS 和
   Linux X11；但 WebKitGTK 不能嵌进别人的 Wayland surface，Linux 上整个应用得跑在 XWayland 上，
   Windows 上 WebView2 会盖住弹出层。还要引入 gtk3-rs（RustSec 标为 unmaintained，glib 0.18 unsound）、
   系统的 webkit2gtk，以及与 rustls 无关的网络栈。放弃。
2. **进程内引擎画成像素**，Wayland、Windows、macOS 行为一致，且能在 GUI 测试里跑。并排接入
   litehtml 与 Blitz，用真实邮件对比：
   - **litehtml**（C++ CSS 2.1 引擎，`litehtml` 0.2.6 绑定，CPU 容器为 tiny-skia + cosmic-text 0.14）：
     表格布局正确。但绑定的容器在开发机（NixOS，装有可变字体的 Noto CJK 和 Maple Mono NF CN）上
     把汉字一律回退到等宽的 Maple Mono（汉字步进 1.2 em），指定 `Noto Sans CJK SC` 也不生效，中文
     字距明显偏大；要用就得自己实现 `DocumentContainer` 和字体选择。它没有读属性的接口，链接只能
     模拟一次点击、由 `on_anchor_click` 回调给出。另需 C++ 编译（Windows 上 MSVC）。
   - **Blitz**：表格、中文排版、字体选择（fontique 走 fontconfig）都正常，纯 Rust，现代 CSS。
     代价是 beta（API 会变、crate 间精确 pin）和 Stylo 的编译时间。
   选 Blitz，litehtml 删除。
3. 没有试的：Servo（自带 JS 和网络栈，嵌邮件太重）、各平台各用一套 webview（三套实现，WPE 在
   crates.io 上没有绑定）、给 `TextView` 补 CSS（上限低）。

## 图标

应用用 [Heroicons](https://heroicons.com)（MIT，2.2.0），只把用到的 SVG 放在
`crates/catmail-gui/assets/heroicons/`（附许可证）并编译进程序（`assets::Hero`）。gpui-kit 组件自带的
Lucide 图标（勾、箭头、关闭、搜索……）在资源加载时按路径换成对应的 Heroicon，整个窗口只有一套图标；
没有对应的（加载动画、面板开关）仍用 gpui-kit 的。测试检查每个 Heroicon 都已嵌入、被替换的路径确实
是 gpui-kit 的图标。

## 已知问题与升级须知

- **可变字体的字重（上游）。** Linux 上 GPUI（`gpui-fast-wgpu` 0.1.10 的 `cosmic_text_system.rs`，
  与 `gpui-pre-wgpu` 0.3.9 的相同）栅格化字形时不设 `wght` 轴坐标，可变字体一律按默认实例渲染。系统
  只有 Noto Sans CJK 可变字体时，中文偏细、加粗无效；只给静态字体时字重正常。上游跟踪 zed-industries/zed#60155（#64123、#64783 同一处）。界面
  文字用自带的静态 Noto Sans 绕开，CJK 只在装有静态字面的字族里挑回退字体（见[字体](#字体linux)）；只有
  可变 CJK 字体的系统，中文仍然没有粗体，用户可改装静态版（如 nixpkgs 的 `noto-fonts-cjk-sans-static`）。
  升级 gpui-kit 时检查这几个 issue：修好后可以去掉自带字体和 `fonts.rs` 的回退挑选。
- **依赖锁定。** `gpui-fast-*` 精确 pin 了 `cocoa = 0.26.0` 和 `unicode-properties = 0.1.3`，锁文件里
  这两个必须保持这些版本；升级 gpui-kit 时对照它自带的 `Cargo.lock`。
