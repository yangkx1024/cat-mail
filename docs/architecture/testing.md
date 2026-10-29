# 测试

[← 架构](../architecture.md)

协议行为先对进程内的假服务器测试（三个平台都跑），再对真实的 Dovecot 与 mailpit 测试（Linux CI 的
测试 job 里与其余测试一起跑）。运行方式见 README"开发"一节。

## 假服务器（`catmail-testkit`）

只作为 dev-dependency。直接按字节写响应，故障可以精确到线上格式。

- **IMAP**：进程内、可修改邮箱内容，明文 / TLS / STARTTLS。BODYSTRUCTURE 和各 section 由每封邮件的原文
  推导（mail-parser），测试邮件就是普通的 `.eml`。SELECT 可写，支持 `UID STORE` / `COPY` / `MOVE`
  （带 COPYUID）/ `EXPUNGE`。像真实服务器一样报告别的连接造成的变化：每条连接记住所选文件夹的样子，
  任何命令的答复里顺带报告新邮件（`* n EXISTS`），IDLE 时（每 10 ms 看一次）报告 EXPUNGE、EXISTS 和
  flags 变化（`FETCH`）；所选文件夹的 UIDVALIDITY 改变后，IDLE 中或收到下一条命令时回 BYE 并断开，
  和 Dovecot 一样。记录当前打开的连接数。可注入的故障：
  - 连接：拉邮件头时断线（第 n 次）或挂起，拉正文时断线、挂起或按闸门放行，关闭当前所有连接
    （IDLE 中的也立即断开），每次 IDLE 一开始就断开、立即用 OK 结束或发一行无法解析的数据，拒绝登录；
  - 响应：重复或倒序的 FETCH、不带 UID 的主动 FETCH、section 回 NIL、比原文多一字节的 RFC822.SIZE、
    STARTTLS 之后的明文注入；
  - 能力与状态：没有 CONDSTORE，关掉 MOVE、UIDPLUS 和 IDLE，UIDVALIDITY 改变；
  - 写操作：拒绝 APPEND（回 NO，或不要 literal 直接回 NO）、APPEND 存下后不答复就断开，拒绝修改、
    修改后不答复就断开（或只在 EXPUNGE 后断开），COPY 之前先往目标投递一封。
- **Gmail**（`ServerState::gmail`）：`X-GM-EXT-1`，按[验证结果](labels.md#验证结果)模拟。邮件在"所有邮件"、
  已删除、垃圾邮件三个位置里，各带 `X-GM-MSGID` / `X-GM-THRID` 和标签；INBOX 等标签邮箱在每条命令（和
  IDLE 的每次查看）之前按"所有邮件"里的标签重建，各有自己的 UID，邮件带着标签时 UID 不变。测试改
  "所有邮件"里的邮件（`set_labels` 像 flags 一样让 MODSEQ 变大），直接改标签邮箱的会被重建覆盖。客户端在
  标签邮箱里的命令作用于邮件本身：flags 是邮件的，`\Deleted` 立即去掉这个标签（在草稿里整封删除），
  MOVE 到"所有邮件"是去掉标签，在两个标签之间 MOVE 是换标签；位置之间 MOVE 时 `X-GM-MSGID` 不变，已删除
  去掉 `\Inbox`，垃圾邮件再去掉 `\Important`。FETCH 支持 `X-GM-*`，在标签邮箱里不列它自己的标签；
  `UID STORE ±X-GM-LABELS`、`UID SEARCH X-GM-MSGID`。测试用 `move_message` 扮演另一个客户端在位置之间
  移动邮件；从 `mailboxes` 里拿掉"所有邮件"就是在 Gmail 设置里把它从 IMAP 隐藏。
- 测试还可以直接在客户端的数据库里加触发器，让某条写入失败，模拟磁盘满（`tests/common` 的 `sql`）。
- **SMTP**：明文 / TLS / STARTTLS，AUTH PLAIN / LOGIN，记录投递的信封和正文（已去掉点填充）。故障：
  421 问候、拒绝登录、SIZE 上限、拒绝指定收件人（4xx / 5xx）、MAIL FROM 回 451 或断线、对结束标记回
  指定的错误码、**收下邮件后不答复就断开**、STARTTLS 之后的明文注入。

## 真实服务器

`testenv/dovecot/run.sh` 以当前用户在临时目录里启动 Dovecot 和 mailpit（来自 flake 的 `testenv`
开发环境，版本随 `flake.lock` 固定）。测试读 `run.sh` 打印的变量，没有就跳过，
`CATMAIL_TESTENV_REQUIRED=1` 时失败；两个测试文件共用 `tests/testenv/`（建客户端和账户、在 Sent 里找
邮件）。

- **IMAP**（`crates/catmail-core/tests/dovecot.rs`）：预置 `testenv/fixtures/` 里的邮件，包括中文
  文件夹、层级文件夹、各种编码、HTML 加内嵌图片与 RFC 2231 文件名；另投递一封超过 2 MiB 的邮件走按
  部分下载。保存到 Sent 时先把 Sent 的 `tmp/` 设为只读让第一次 APPEND 失败，第二次就会先按 Message-ID
  搜索，所以 APPEND 和 `SEARCH HEADER` 都在真实服务器上验证过（因此不能以 root 运行）。操作队列也走
  一遍：改 flags、MOVE（同步后不重复拉取）、永久删除，结果直接看 maildir 的文件名和内容。后台同步
  以单独的 `push` 用户登录，在它 IDLE 时直接往 maildir 投递，新邮件不经同步出现在本地（Dovecot 用
  inotify 发现，通常不到一秒）。草稿同步用单独的 `drafts` 用户（不影响对预置邮件的同步断言）：上传、
  修改后替换、删除，看 Drafts 的 maildir 里只有一份且带 `DS` 标记。
- **SMTP**（`tests/submission.rs`）：Dovecot 的 submission 服务（STARTTLS 与隐式 TLS 各一个端口，
  `run.sh` 用 openssl 生成 127.0.0.1 的自签名证书，测试只信任它；`submission_max_mail_size` 为 1 MiB）
  转发给 mailpit，测试从 mailpit 的 HTTP API 读回：信封收件人（密送只在信封里）、原文与 Sent 里的副本
  逐字节一致（服务器只在前面加 trace 头）、收件人被拒（mailpit 只收 `@example.com`）、超过 SIZE、密码
  错误。mailpit 存信时会给只在信封里的收件人加一行 `Bcc:`，所以"邮件里没有 Bcc"看的是 Sent 的副本。
  每次运行的邮件按 Message-ID 或带时间戳的主题区分，可以对同一组服务器重复运行；它以单独的 `sender`
  用户登录，和 `dovecot.rs` 互不影响，可以同时跑。

## GUI

UI 集成测试（`#[gpui_kit::test]`，GPUI 的测试平台，无需显示器或 GPU，三个平台都跑）用假 IMAP 服务器
（INBOX 加载 `testenv/fixtures/` 和 `crates/catmail-gui/fixtures/` 的邮件，另有 Archive、Drafts、Sent、
Trash）和假 SMTP 服务器，驱动真实的窗口：点击、键盘、切换 HTML，打开即已读并经队列到服务器、归档后
打开下一封、在写信窗口回复并发出、未改动的草稿关闭即删除、地址写错时不关窗、在设置里换主题，以及
"另一处同步后窗口靠事件更新且保持选中"。客户端与应用一样在后台发信，但不在后台同步，由测试决定何时
看服务器。引擎在自己的线程上运行，GPUI 的测试调度器看不到，所以
测试在 `run_until_parked` 之间按真实时间等待（`test_support::wait_until`）。
测试打开"减少动态效果"（`set_reduce_motion(true)`）：对话框会滑入，命中测试跟着移动，在慢的 CI 机器上
点对话框里的按钮会落在上一帧的位置；测试调度器下 gpui-kit 不读系统设置，只能由测试自己设。
测试的时钟只在 `advance_clock` 时走，所以主窗口的事件节流间隔（`refresh_interval`）在测试里设为零。
断言"什么都没发生"时等一个正面的信号（窗口显示了原因、第二次打开同步返回），不固定 sleep。
测试要点 gpui-kit 自己生成的元素 id（SidebarMenu 的第几项）时，只经 `settings::dialog::section_item`
取得，升级 gpui-kit 改了命名只需改这一处；确认框的按钮是我们自己的，id 是 `layout::CONFIRM` /
`layout::CANCEL`。

窗口里的决策逻辑抽成纯函数做表驱动测试，不必经过窗口：列表刷新后选中去哪（`message_list::reselect`）、
哪个事件重读什么及节流（`mailbox::refresh`）、写信窗口关闭时等待 / 提示 / 关闭 / 先保存（`composer::lifecycle`
的 `closing`、`steps`）。

`mailbox::tests::screenshots`（`mailbox/tests/screenshots.rs`，`#[ignore]`）用 WGPU 离屏渲染把几封邮件、每个内置主题、设置和写信窗口
截成 PNG，供人检查排版。

## 覆盖不到的

进程在同步或发信中途被杀：同步用"断线后重新同步"近似（事务边界相同）；发信的重启恢复由单元测试直接
构造中断时的数据库状态。
