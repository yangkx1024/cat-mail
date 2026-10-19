# GUI

[← 架构](../architecture.md)

`catmail-gui`，基于 `gpui-kit`（longbridge）：虚拟列表、Table、编辑器、原生 HTML 渲染，无 WebView。
gpui-kit 构建在 `gpui-pre-*` 快照上并精确 pin 版本，应用只依赖 `gpui-kit`（`use gpui_kit::*` 就是
GPUI），不直接依赖 `gpui`，否则会链接两份 GPUI。gpui-kit 的编码与设计规范作为 skill 放在
`crates/catmail-gui/.claude/skills/`，改 GUI 前先读。

目前是最小读信窗口（`crates/catmail-gui/src/`）。

## 结构

- `Mailbox`：根视图，侧栏的账户与文件夹树、同步按钮、三栏 `h_resizable`；登录页打开时整个替换三栏。
- `AddAccount`：登录页。
- `MessageList`：gpui-kit `List` 的 delegate，选择、键盘导航、滚动到底分页加载由 List 负责。
- `Reader`：读信。
- 纯逻辑（文件夹树、日期格式、HTML 的图片 / 链接策略）在 `folders.rs`、`format.rs`、`html.rs`，
  不依赖 GPUI，单测覆盖。

## 运行时与事件

`MailClient::builder(cfg).open()` 自建 tokio runtime；视图只在 `cx.spawn` / `spawn_in` 中 await 门面
方法，从不碰 tokio。事件的 `broadcast::Receiver` 不依赖 reactor，直接在 GPUI 的 executor 上 `recv`；
`Lagged` 时整体刷新，`Closed` 时退出。

- `FoldersChanged` / 账户增删：重读侧栏。
- `MessagesChanged`：重读侧栏（计数）；是当前文件夹时重读已显示的行，按 MessageId 保持选中。选中的
  邮件刷新后不见了（被删除、UIDVALIDITY 改变）时，列表发出 `ListEvent::Cancel`（与按 Escape 相同），
  读信区随之清空。
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

## 登录页

没有账户时自动打开（不能取消），侧栏"Add account…"也能打开。先调用 `MailClient::check_login`
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

gpui-kit 默认资源只含组件用到的图标，其余 Lucide 图标用 `icon_assets!` 只嵌入用到的几个，与默认资源
组合（`assets.rs`，有测试检查视图用到的图标都在）。

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
