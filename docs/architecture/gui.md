# GUI

[← 架构](../architecture.md)

`catmail-gui`，基于 `gpui-kit`（longbridge）：虚拟列表、Table、编辑器、原生 HTML 渲染，无 WebView。
gpui-kit 构建在 `gpui-pre-*` 快照上并精确 pin 版本，应用只依赖 `gpui-kit`（`use gpui_kit::*` 就是
GPUI），不直接依赖 `gpui`，否则会链接两份 GPUI。gpui-kit 的编码与设计规范作为 skill 放在
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
  format.rs html.rs  日期、地址、邮件主题的显示格式；HTML 渲染策略
  layout.rs          共用的工具栏、右栏页面、名称与值的行、附件块
  mailbox/           主窗口
    mod.rs           状态（账户、Place）、引擎事件、读侧栏、搜索、渲染与 Action 分派
    commands.rs      对邮件的命令（标记、归档、删除、移动）、写信、同步、被拒改动的通知
    sidebar.rs panes.rs draft.rs outbox.rs   侧栏；三栏与邮件工具栏；草稿、发件箱的右栏
    message_list/    中栏列表（mod.rs：来源、加载、选中；rows.rs：三种行的样子）
    folders.rs       文件夹分区（纯逻辑）
    move_to.rs add_account.rs   "Move to…"对话框；登录表单
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
Junk、Trash），名字用角色名而不是服务器上的名字（`已发送` 显示为 Sent）；不论嵌套在哪（Dovecot 的
`INBOX.Sent`）都提到这一层，原父级没有其他内容时去掉（`folders::AccountFolders`）。其余文件夹按
服务器的层级跟在后面。

- **Drafts 是本机草稿**，不是服务器的 Drafts 文件夹：那里是本机草稿的副本，按邮件列出会重复、也
  不能编辑。代价是其他客户端写的草稿在 GUI 里看不到（CLI 的 `catmail ls Drafts` 仍能看到）。
- **Outbox 只在有待发邮件时出现**，有被拒绝或未确认的条目时带警告图标。
- Sent、Junk、Trash 不显示未读数。底部一行说明连接问题（同步中、登录被拒、连不上、发不出去）。

## 读与改

- **打开即已读**（设置可关）：列表选中一封未读邮件就 `set_flags(SEEN)`，经操作队列到服务器。只在用户
  选中时标记，刷新后恢复选中不算。
- **归档 / 删除 / 移动后打开下一封**：命令先让列表记下要移走的邮件（`expect_removal(id)`），刷新后
  它仍是选中的那封却不在了，就选中同一位置的那一行；服务器那边消失的邮件仍然只清空读信区。标记按邮件
  身份认：用户换了选中，标记即作废，之后别的邮件消失不会被当成"移走后打开下一封"。
- 已在 Trash（或账户没有 Trash）时删除是永久的，先用 AlertDialog 确认；其余可逆的操作不确认。
- 服务器拒绝的改动（`PendingOpsChanged` 后 `list_pending` 里非 pending 的）以通知说明一次，随即
  `dismiss_pending`，避免重复提示。

## 写信窗口

每封信一个窗口（`OpenComposers` 全局按草稿记录窗口，再次打开同一草稿时激活已有窗口；读草稿是异步的，
建窗口前再查一次，连点两次 Edit 也只有一个窗口）。

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

客户端开启 `send_in_background`。条目的状态决定右栏给出的选择：排队中可以"Send now"
（`flush_outbox`）或"Back to drafts"（`cancel_outbox`）；被拒绝的可以改回草稿或重试；已发出但
没存进 Sent 的可以重试保存或放弃；**发出后未确认（unknown）的从不自动重发**，由用户选"Mark as sent"、
"Send again…"（确认可能重复）或"Back to drafts"。

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
`Lagged` 时整体刷新，`Closed` 时退出。

客户端开启了后台同步（`sync_in_background`）：服务器推送的新邮件同样经 `MessagesChanged` 出现。启动和
添加账户时仍手动同步一次，把错误显示成通知；后台同步的错误只记日志。

- `FoldersChanged` / 账户增删：重读侧栏。
- `MessagesChanged`：重读侧栏（计数）；列表显示的是这个文件夹或搜索结果时重读已显示的行，按行的
  身份保持选中。选中的邮件刷新后不见了（被删除、UIDVALIDITY 改变）时，列表发出 `ListEvent::Cancel`
  （与按 Escape 相同），读信区随之清空；用户自己移走的除外（见"读与改"）。
- `DraftsChanged`：只重数该账户的草稿（`count_drafts`，不读正文）——写信窗口每次停顿都保存，侧栏
  别的内容不会因此变化；正显示该账户的草稿时刷新列表。`OutboxChanged`：重读侧栏，正显示该账户的发件箱
  时刷新列表。`AccountStatusChanged`：更新侧栏底部的连接说明。`PendingOpsChanged`：报告被拒绝的改动。
- 同步期间事件比读取快得多，所以读侧栏和刷新列表都合并请求：同一时间最多一次读取，期间再有事件只记
  一个"过期"标记，完成后补读一次。只替换 GPUI 侧的任务不够，已交给引擎的读取照样会做完。切换文件夹
  则直接替换任务，带代际号丢弃过期结果。

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
  - 每个 URL 只解析一次后缓存（视图每帧都会问）。"远程图片未加载"的提示由
    `catmail_core::html::image_sources` 判断，复用清洗用的同一个 ammonia 配置按解析结果列出
    `img src`，在后台线程运行。内嵌图片在正文显示后再逐个取。
- **链接。** 只把 `http` / `https` / `mailto` 交给系统打开；其他链接点了无反应（gpui-kit 默认会
  `open_url` 任何链接）。
- **在浏览器打开。** 把清洗后的 HTML 写成独立页面：`catmail_core::html::embed_inline_images` 把内嵌
  图片换成 `data:` URL（浏览器不认 `cid:`；用读信窗口已加载的图片），加 CSP
  `default-src 'none'; img-src data:; style-src 'unsafe-inline'` 和 `no-referrer`。
  - 替换在 proto 里随清洗的同一次解析完成，`data:` URL 也由 proto 构造（MIME 类型只留合法字符）：
    ammonia 不再检查属性过滤器返回的值，所以不接受调用方给的任意字符串。
  - 页面写在数据目录的 `handoff/`（`Config::handoff_dir`）并保留，因为浏览器在函数返回后才读它；
    引擎下次打开时清空。不放系统临时目录：那里没人清理，整封邮件会一直留在磁盘上。
- **附件。** 列出非内嵌部分，点击后弹系统保存对话框（文件名经 `safe_filename`），需要时先下载。
- **头部。** 发件人头像是姓名首字，底色按地址从主题的六个色相里挑（同一个人颜色固定）；收件人一行只
  列名字，"Details"展开完整的 From / To / Cc / Reply-To / Date。标题、头部和正文在同一个滚动区里，
  正文限宽 48rem 便于阅读。

## 登录页

没有账户时自动打开（不能取消），Settings › Accounts 的"Add account…"也能打开。先调用 `MailClient::check_login`
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
- gpui-kit 块元素表里缺、却能通过清洗的块元素（`center`、`dd`、`dt`、`hgroup`，对照 gpui-kit 0.7.3）
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

- **可变字体的字重（上游）。** Linux 上 GPUI（`gpui-pre-wgpu` 0.3.9 的 `cosmic_text_system.rs`）栅格化
  字形时不设 `wght` 轴坐标，可变字体一律按默认实例渲染。系统只有 Noto Sans CJK 可变字体时，中文偏细、
  加粗无效；只给静态字体时字重正常。上游跟踪 zed-industries/zed#60155（#64123、#64783 同一处）。我们
  不绕开（打包静态 CJK 字体每个字重数 MB），用户可改装静态版（如 nixpkgs 的
  `noto-fonts-cjk-sans-static`）。升级 gpui-kit 时检查这几个 issue。
- **依赖锁定。** Cargo 解析锁文件时会把 `dep?/feature` 里的可选依赖也算进去，开启
  `gpui-kit/test-support` 会让 `gpui-fast-*` 进入 `Cargo.lock`（不编译）。它们精确 pin 了
  `cocoa = 0.26.0` 和 `unicode-properties = 0.1.3`，锁文件里这两个必须保持这些版本；升级 gpui-kit 时
  对照它自带的 `Cargo.lock`。
