# 一轮发送按 SMTP 登录分组共用连接，各组并发

- 日期：2026-10-23
- 类型：重构

## 背景

Issue #92 和 #97，都是 #80（发件身份）之后 `send.rs` 的 `pass` 留下的：

- #92：每个身份各开一条 SMTP 连接、各查一次 keyring。同一服务器、同一用户名的几个地址（Gmail 的
  send-as 别名）是几个身份，outbox 里同时有它们的信时每个都要单独 TCP + TLS + AUTH。
- #97：一轮按 outbox id 顺序逐条处理，轮到某个身份的第一封才连接。一台服务器不回答时，排在后面的其他
  身份的信都要等它超时（每次读写 60 秒）；整轮持有发送锁，期间别的 `catmail send` 也要等。QUIT 也是
  逐条连接发完才去存 Sent。

## 决策

- 分组的键是（服务器、用户名、密码），不只是（服务器、用户名）。除了账户自己的，每个身份都有自己的
  keyring 条目，同一登录的别名里可能有一个密码过时了：只按服务器和用户名分组时，用哪个密码取决于哪个
  身份排在前面，过时的那个会挡住其他身份。密码相同才共用，不同就各自连接、各自记失败。keyring 条目按
  键去重，每个只读一次。
- 共用连接的结果（登录失败、SIZE）记到组里每个身份上，失败次数和账户状态仍按身份记。一组推迟到同一时刻，
  间隔按组里失败次数最多的身份算，不给组里每个身份各算一个时间。
- 不按 keyring 条目（`login_key`）分组：账户之后添加的身份各有自己的条目，同一服务商的别名之间、和账户
  自己的身份之间都不会相同，#92 就落空了。
- 各组在这一轮的任务里用 `join_all` 并发，不用 `JoinSet::spawn`：账户被删除（`Permit::run` 取消）时
  整轮随 future 一起被丢弃；spawn 出去的任务要到下一次 poll 才中止，发送锁释放后可能还有一组在发。为此
  新增依赖 `futures-util`（已在依赖树里，只开 `alloc`）。
- 一组中途出错时不取消其他组，让它们做完、记下结果，再返回第一个错误；报告的条目按 id 排序，和原来的
  顺序一致。`unknown` 在结束标记之前 `write_durable` 的规则在每组的顺序流程里不变。
- 重试之前没存进 Sent 的那一段仍在各组都结束之后。

## 方案

- `connection.rs`：`connect_smtp` 换成 `smtp_logins`（一次读出各身份，按键去重读密码，分组成
  `SmtpLogin`）和 `SmtpLogin::connect`（连接，在一次写入里给组里 SIZE 变了的身份记下）。`Sender` 去掉，
  `sender` / `draft_sender` 直接返回地址。
- `send.rs`：`pass` 先 `routes` 分组，再对每组并发 `send_route`；`connected` 把连接结果记到组里每个
  身份，`Down::postpone` 推迟连不上的组的条目。每组发完各自 QUIT。
- testkit：`SmtpFaults::greeting_gate`，拿到许可前不发问候，模拟不回答的服务器。
- 文档：`docs/architecture/sending.md` 的发件身份、附件上限和重试两节。
- 测试（`crates/catmail-core/tests/identities.rs`）：`identities_with_one_login_share_a_connection`
  （三封信一条连接、按入队顺序，SIZE 也记到别名上），`a_login_with_another_password_connects_on_its_own`，
  `a_server_that_doesnt_answer_holds_up_only_its_own_messages`（另一台服务器的信在它问候之前就发出）。
  第一和第三个在改动前失败。
