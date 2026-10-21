# GUI

[← 架构](../architecture.md)

`catmail-gui`，基于 `gpui-kit`（longbridge）：虚拟列表、Table、编辑器、原生 HTML 渲染，无 WebView。
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
  main.rs            启动：设置、主题、快捷键、主窗口
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
    move_to.rs add_account.rs   "Move to"对话框；登录表单
    tests/           驱动窗口的集成测试，按读信、写信、设置分文件，另有截图
  reader/            读信：mod.rs 状态与操作，message.rs 打开的邮件，images.rs 内嵌图片，view.rs 渲染
  composer/          写信窗口：mod.rs 打开与编辑，lifecycle.rs 保存 / 发送 / 关闭，view.rs 渲染
  settings/          mod.rs 偏好（settings.json）；dialog/ 设置对话框（外观与主题、账户分文件）
  theme/             palette / derive / builtin / import/（纯逻辑：调色板、推导、内置、导入）；
                     appearance.rs 可选主题与应用；colors.rs 应用自己的颜色
```

- `Mailbox`：根视图。状态是账户列表（每个账户的文件夹、草稿数、发件箱、连接状态）和侧栏选中的位置
  `Place`（文件夹、本机草稿、发件箱）；登录页打开时整个替换三栏。对邮件的命令（回复、归档、删除、
  移动、标记……）都是它的方法（`commands.rs`），工具栏按钮、菜单和快捷键调用同一个方法或 Action。
- `MessageList`：gpui-kit `List` 的 delegate，一个列表显示四种来源（`Source`）：文件夹（分页）、
  搜索结果、本机草稿、发件箱。行按身份（`RowId`）保持选中。搜索框停顿 200 ms 才搜索，不是每个键都
  读一遍。
- 纯逻辑（文件夹分区、日期格式、HTML 策略、设置文件、主题推导与导入）不依赖窗口，单测覆盖。

## 侧栏

每个账户先列有角色的文件夹，顺序固定（Inbox、Drafts、Outbox、Sent、Archive、All Mail、Flagged、
Junk、Trash），名字用角色名（`FolderRole::label`）而不是服务器上的名字（`已发送` 显示为 Sent）；
图标和顺序在 `folders::RoleInfo` 一张表里；不论嵌套在哪（Dovecot 的
`INBOX.Sent`）都提到这一层，原父级没有其他内容时去掉（`folders::AccountFolders`）。其余文件夹按
服务器的层级跟在后面。

- **Drafts 是本机草稿**，不是服务器的 Drafts 文件夹：那里是本机草稿的副本，按邮件列出会重复、也
  不能编辑。代价是其他客户端写的草稿在 GUI 里看不到（CLI 的 `catmail ls Drafts` 仍能看到）。
- **Outbox 只在有待发邮件时出现**，有被拒绝或未确认的条目时带警告图标。
- Sent、Junk、Trash 不显示未读数。底部一行说明连接问题（同步中、登录被拒、连不上、发不出去）。

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

## 读与改

- **打开即已读**（设置可关）：列表选中一封未读邮件就 `set_flags(SEEN)`，经操作队列到服务器。只在用户
  选中时标记，刷新后恢复选中不算。
- **归档 / 删除 / 移动后打开下一封**：命令先让列表记下要移走的邮件（`expect_removal(id)`），刷新后
  它仍是选中的那封却不在了，就选中同一位置的那一行；服务器那边消失的邮件仍然只清空读信区。标记按邮件
  身份认：用户换了选中，标记即作废，之后别的邮件消失不会被当成"移走后打开下一封"。
- 已在 Trash（或账户没有 Trash）时删除是永久的，先用 AlertDialog 确认（`layout::confirm_danger`）；
  其余可逆的操作不确认。是否永久由 `DeleteOutcome` 判断，归档目标由 `archive_folder` 判断，都基于
  账户的文件夹列表而不是侧栏的顺序：有两个 Trash 时，侧栏先列的不一定是引擎移入的那个，而在任一
  Trash 里删除都是永久的。
- 服务器拒绝的改动：`PendingOpsChanged` 后 `dismiss_settled` 在一个写事务里取出并忘掉所有失败或冲突的
  改动，逐条以通知说明，不会重复提示。

## 写信窗口

每封信一个窗口（`OpenComposers` 全局按草稿记录窗口，再次打开同一草稿时激活已有窗口；读草稿是异步的，
正在读的草稿也记下，再次打开直接返回，连点两次 Edit 也只有一个窗口。读草稿的任务挂在应用上而不是发起的
窗口上：那个窗口先关了也不会让草稿一直处于"正在打开"）。

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
- 窗口边框的关闭按钮经 `on_window_should_close` 走同一流程（需要先保存时返回 false，存完自己关）。
- Ctrl Enter 绑在 `Composer > Input` 上直接是 `Send`：多行输入框自己处理时会先插一个换行，再随信发出。
  附件可选文件或拖进窗口（`ExternalPaths`）。
- **退出**（Ctrl Q）先让每个写信窗口走关闭流程（存下最后的输入），最后一个关掉后才退出；GPUI 的
  `quit` 不经过窗口的关闭回调。有窗口关不了（地址要改、保存失败）时留下它，也不退出。

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

`MailClient::builder(cfg).open()` 自建 tokio runtime；视图只在 `cx.spawn` / `spawn_in` 中 await 门面
方法，从不碰 tokio。事件的 `broadcast::Receiver` 不依赖 reactor，直接在 GPUI 的 executor 上 `recv`；
`Lagged` 时整体刷新（侧栏、列表，并查一次被拒绝的改动），`Closed` 时退出。

客户端开启了后台同步（`sync_in_background`）：服务器推送的新邮件同样经 `MessagesChanged` 出现。启动和
添加账户时仍手动同步一次，把错误显示成通知；后台同步的错误只记日志。后台同步在两次完整同步之间只盯
INBOX，所以打开一个文件夹时同步它（`MailClient::sync_folder`，一分钟内同步过的跳过，见
[同步 / 打开的文件夹](sync.md#打开的文件夹)）；失败只记日志，连不上服务器已由侧栏底部说明。

哪个事件重读什么由 `refresh::Refresh::after` 一张表决定：

- **侧栏**一次读完：`MailClient::overview` 在一个读事务里取所有账户的文件夹（含计数）、草稿数和发件箱，
  不是每个账户 2 次查询、各自一次运行时往返。连接状态不在数据库里，不进快照：结果落到窗口时才取
  `account_status`，所以不会比它的事件旧。账户增删、`FoldersChanged`、`MessagesChanged`（计数）、
  `DraftsChanged`、`OutboxChanged` 都重读侧栏。草稿数以前单独 `count_drafts`，但它的结果与侧栏的读取
  互相没有先后，晚到的旧值会盖住新的；侧栏本身只有一次一读，最后一次读取一定在最后一次变化之后。
- **列表**：`MessagesChanged` 且列表显示这个文件夹，或显示搜索结果（搜索跨所有账户和文件夹，任何
  文件夹变了都可能影响结果）；`DraftsChanged` / `OutboxChanged` 且显示该账户的草稿 / 发件箱。重读已
  显示的行，按行的身份保持选中（`message_list::reselect`）。选中的邮件刷新后不见了（被删除、
  UIDVALIDITY 改变）时，列表发出 `ListEvent::Cancel`（与按 Escape 相同），读信区随之清空；用户自己
  移走的除外（见"读与改"）。
- `AccountStatusChanged`：直接更新侧栏底部的连接说明。`PendingOpsChanged`：报告被拒绝的改动，同一时间
  只查一次，期间再有事件补查一次（忘掉它们本身也发这个事件）。
- **节流**（`refresh::Throttle`）：一次读取之后 200 ms 内的事件先攒着，间隔结束时合在一起读一次，然后
  再等一个间隔；一个间隔里没有事件就回到空闲，下一个事件立即读。单个改动（标记已读）马上显示，一次
  同步的几十个事件每 200 ms 最多读一轮。不按"同步进行中"去抖：后台同步不经过窗口的 `syncing` 计数。
- 节流之下，读侧栏和刷新列表各自还合并请求（侧栏和被拒绝的改动用 `refresh::Coalesce`）：同一时间最多一次读取，期间再有事件只记一个"过期"标记，
  完成后补读一次。只替换 GPUI 侧的任务不够，已交给引擎的读取照样会做完。切换文件夹则直接替换任务，
  带代际号丢弃过期结果。
- **搜索结果分页**，和文件夹一样每页 200 条，滚到底再取下一页（`SearchQuery::offset`），不再在
  500 条处静默截断。

## 读信

- **正文。** 默认显示纯文本（`SelectableText`），有 HTML 部分时可切换，选择在切换邮件时保留。打开时把
  正文、标题、头部和附件列表一次准备好（`OpenMessage`），正文作为同一个 `SharedString` 每帧交给视图。
  HTML 用 `gpui_base::text::TextView`：只有 base 层暴露 `image_source`，主题样式由 component 安装的
  `TextViewDefaults` 提供。
- **图片。** `image_source` 决定每张图：
  - `cid:` 按 `content_id_from_url`（与引擎判断"内嵌"同一条 RFC 2392 规则）查已加载的内嵌部分；
  - `data:` 用 `data-url`（gpui-kit 自己用的同一个 WHATWG 解析器）解码；
  - 其他（远程、`file:`、相对路径）返回立即失败的 `ImageSource::Custom`，不发任何请求；
  - **SVG 一律不显示**：GPUI 用默认选项把 SVG 交给 usvg，它会把嵌套的 `<image href>` 当本地路径直接
    读文件，绕过 `image_source`，邮件就能显示本地图片，或读 `/dev/zero` 耗尽内存。
  - 显示与否只有一条规则（`html::Carried::of`：内嵌部分或 `data:` 图片），`image_source` 和"远程图片
    未加载"的提示都用它。`data:` 图片只解码一次后缓存（视图每帧都会问）。"远程图片未加载"的提示由
    `catmail_core::html::image_sources` 判断，复用清洗用的同一个 ammonia 配置按解析结果列出
    `img src`，在后台线程运行。内嵌图片在正文显示后再逐个取。
- **链接。** 只把 `http` / `https` / `mailto` 交给系统打开；其他链接点了无反应（gpui-kit 默认会
  `open_url` 任何链接）。
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

没有账户时自动打开（不能取消），Settings › Accounts 的"Add account"也能打开。先调用 `MailClient::check_login`
（连接、登录、LOGOUT，不写数据库也不写密钥环），成功后才 `add_account`，再同步这个账户；密码或服务器
填错不会留下账户行或孤立的密钥环条目。

- 登录进行中"Cancel"不可用：`add_account` 一旦开始就会写入（引擎的阻塞任务不随窗口的任务取消），
  此时离开只会让新账户错过首次同步。
- 空的端口和用户名取与 CLI 相同的默认值；表单只检查端口格式，其余由 `check_login` 里与 `add_account`
  相同的校验负责。凭据由 `auth::new_account_credentials` 按 `AuthKind` 构造，加 OAuth2 时只改这一处。

## 展开布局表格

gpui-kit 的表格单元格只是一个行内段落（gpui-base `text/format/html.rs` 的 `parse_table_cell`），而
营销邮件普遍用 `<table width=600><tr><td>` 套整个正文，标题、段落、内层表格、列表会被压成一段。所以
交给 `TextView` 的 HTML 先经过 `catmail_core::html::unwrap_layout_tables`（proto 的 `mime/layout.rs`）：

- 某个单元格里有块元素，或者每行至多一个单元格的表格，连同 `tbody` / `tr` / `td` 改成嵌套的 `div`，
  属性原样保留；其余是数据表格，保持不变（单元格里只有行内内容，gpui-kit 能排）。多列布局因此变成
  上下排列，对窄的读信区反而合适。
- gpui-kit 块元素表里缺、却能通过清洗的块元素（`center`、`dd`、`dt`、`hgroup`，对照 gpui-kit 0.8.0）
  不论在哪都改成 `div`，否则同样被压平。
- 没有可改的内容时直接借用原字符串，不解析。
- 只用于读信区：数据库里和"在浏览器打开"用的都是原样清洗后的 HTML，展开在打开邮件时于后台线程做。
- 实现：清洗后的 HTML 从 DOM 序列化而来，标签必然正确嵌套，所以用 html5ever 的 tokenizer 读成记号流
  （不用 tree builder，它会搬动内容），一遍判断每个表格，再一遍用 html5ever 的 serializer 写出。
  html5ever 与 ammonia 同版本；ammonia 的 DOM 要 `--cfg ammonia_unstable` 才能用，所以没走那条路。
  结果再过一遍同一个 sanitizer。
- 这是绕开 gpui-kit 的限制。升级 gpui-kit 时对照 `parse_table_cell` 和它的块元素表：单元格能放块
  元素后删掉表格部分，表里补上的元素也从 `INLINE_BLOCKS` 去掉。嵌入 `wry` 仍是备选。

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
