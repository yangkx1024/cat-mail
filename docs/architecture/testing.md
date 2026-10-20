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
  flags 变化（`FETCH`），UIDVALIDITY 改变时回 BYE 并断开。记录当前打开的连接数。可注入的故障：
  - 连接：拉邮件头时断线（第 n 次）或挂起，拉正文时断线、挂起或按闸门放行，关闭当前所有连接
    （IDLE 中的也立即断开），每次 IDLE 一开始就断开，拒绝登录；
  - 响应：重复或倒序的 FETCH、不带 UID 的主动 FETCH、section 回 NIL、STARTTLS 之后的明文注入；
  - 能力与状态：没有 CONDSTORE，关掉 MOVE、UIDPLUS 和 IDLE，UIDVALIDITY 改变；
  - 写操作：拒绝 APPEND（回 NO，或不要 literal 直接回 NO）、APPEND 存下后不答复就断开，拒绝修改、
    修改后不答复就断开。
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

UI 集成测试（`#[gpui_kit::test]`，GPUI 的测试平台，无需显示器或 GPU，三个平台都跑）用假服务器加载
`testenv/fixtures/` 和 `crates/catmail-gui/fixtures/` 的邮件，驱动真实的窗口：点击、键盘、切换 HTML，
以及"另一处同步后窗口靠事件更新且保持选中"。引擎在自己的线程上运行，GPUI 的测试调度器看不到，所以
测试在 `run_until_parked` 之间按真实时间等待（`test_support::wait_until`）。

`mailbox::tests::screenshots`（`#[ignore]`）用 WGPU 离屏渲染把几封邮件截成 PNG，供人检查排版。

## 覆盖不到的

进程在同步或发信中途被杀：同步用"断线后重新同步"近似（事务边界相同）；发信的重启恢复由单元测试直接
构造中断时的数据库状态。
