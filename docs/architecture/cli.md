# CLI

`catmail-cli` 只依赖 `catmail-core`，借用 `#[tokio::main]` 的 runtime 打开 `MailClient`。除
`catmail watch` 外不开后台同步和后台发信：改动、草稿、发信都在命令里当场做完，做不完的留在队列里。

## 代码结构

```
src/
  main.rs        clap 的顶层 `Cli` / `Command`，打开 MailClient，分发，按错误决定退出码
  cmd/           每组命令一个文件，各自带 clap 参数类型：account、sync、search（ls / search）、
                 read（read / attachment）、ops（mark / mv / archive / rm / ops）、
                 compose（compose / reply / forward / draft）、outbox（send / outbox）、watch、gc；
                 mod.rs 是它们共用的查找（账户、分页游标、文件夹名）
  output.rs      给人看的输出：转义陌生人给的文本、对齐的表格、各命令的表格和信件视图
  json.rs        给脚本的 `--json` 输出
  exit.rs        退出码
  files.rs       把附件写到磁盘
```

## `--json`

`--json` 的结构在 `json.rs` 里逐个写出，不直接序列化 `catmail-types`：types 里改个字段名、
加个内部字段都不应该悄悄改变脚本读到的东西，而结构改了 `json.rs` 必须跟着改（枚举是穷尽匹配，
types 加变体会编译失败）。

- 每次输出是一个对象，带 `"schema_version": 1`。删除字段、改名或改变含义时加一；只增加字段不加。
  列表放在对象的一个键下（`{"messages": [...]}`），以后可以并列加分页等信息。
- 时间是 UTC 的 RFC 3339 字符串；标记是单词数组（`"flags": ["seen", "flagged"]`），状态和类别是
  小写单词；ID 是整数，引用别的对象用对象名作键（`"account": 1`、`"folder": 3`）。
- `catmail sync` 失败的账户也在输出里，带 `error: {kind, message}`，`ops flush` 等回放没做完的账户
  同样；`catmail read --save` 另有
  `saved`（实际写入的路径）；`catmail watch --json` 每行一个对象，用 `event` 区分类型。
- `json.rs` 的快照测试固定了邮件、草稿、outbox 条目、改动和事件的完整结构；它失败说明输出变了。

## 退出码

| 码 | 含义 |
|---|---|
| 0 | 完成 |
| 1 | 失败：服务器拒绝、找不到、输入有误、本地出错…… |
| 2 | 用法错误（clap） |
| 3 | 没做完，但什么也没丢、也没被拒绝：留在队列里，`catmail sync` / `ops flush` / `outbox flush` 再做 |
| 4 | 有邮件可能已送达、也可能没有（outbox 的 `unknown`），只有用户能判断 |

命令以 `exit::Exit { outcome, message }` 错误表达 3 和 4，其他错误都是 1。一次命令有多种结果时取
最需要用户处理的：4 > 1 > 3，所以 3 只在"只是还没轮到"时出现，脚本可以放心地稍后重跑。

- `mark` / `mv` / `archive` / `rm` / `ops flush`：连不上服务器（`ReplayReport.error` 的 `transient`，
  改动都还在队列里）是 3；登录被拒绝也留着改动，但重跑没用、要先改密码，所以是 1；服务器拒绝了改动或
  草稿是 1，断线之前拒绝的也是。
- `send` / `outbox retry` / `mark-sent` / `outbox flush`：`queued` / `sending` 是 3，`failed` 是 1，
  `unknown` 是 4；`accepted`（已发出、只是没存进 Sent）算成功。连 SMTP 服务器失败时邮件都推迟、仍是
  `queued`，所以还看这次 flush 的 `FlushReport.errors`：经这台服务器发的信，问题不是暂时的（登录被拒绝、
  不支持 STARTTLS）时是 1；按身份判断，一个身份暂时连不上不会掩盖另一个要先改的。不读账户状态：那是全局的，可能是别的发送留下的。`outbox flush` 只看这次处理的条目：
  `unknown` 和 `failed` 的等用户处理，不会被它重试，也就不影响它的退出码。
- `sync` 有账户失败是 1：同步本身没有要排队的东西。

## 添加账户

`account add` 用 `add_account(.., Verify::Login)`（与 GUI 的添加账户对话框相同），IMAP 和 SMTP 都登录成功才写
keyring 和账户行，拼错的密码不会留下任何东西；SMTP 失败的错误以 "outgoing mail server (SMTP)" 开头，分得清
是哪一个。`--no-verify` 换成 `Verify::Skip`，用于离线时或脚本里的本地测试服务器；这时密码错误要到第一次
`sync` 或发信才发现。

`identity add` 给账户加一个发件身份（另一个地址，经它自己的 SMTP 服务器和登录发信，如 Resend），同样
用 `add_identity(.., Verify::Login)` 先登录，`--no-verify` 跳过；`identity list`（默认的标 `*`、
排在最前）、`identity default`、`identity rm`（默认的不能删）。`compose`、`reply`、`forward`、`draft edit`
的 `--from` 按 id 或地址（`identity_by_address`，几个身份地址相同时取默认的或较早的）选身份；不给时新信用
默认身份，`reply` 用原信寄给的那个地址。`reply`、`forward` 先读原信得知账户，把身份连同 `--body` 和
`forward` 的收件人一起交给 `reply` / `forward`，草稿一次建成，不再事后 `update_draft`。`account list` 的 SMTP 列和 `--json` 的 `smtp` 是默认身份的服务器。

## 写文件

两个命令都不覆盖已有文件：路径是用户给的，但内容来自陌生人，误覆盖不可挽回。`read --save DIR`
遇到重名时加编号（`a (1).pdf`）；`attachment --output PATH` 指定了确切的名字，已存在（包括符号链接，
`create_new` 不跟随）就报错，要覆盖可以写到 stdout 再重定向。
