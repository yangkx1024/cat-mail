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
- `catmail sync` 失败的账户也在输出里，带 `error: {kind, message}`；`catmail read --save` 另有
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

- `mark` / `mv` / `archive` / `rm` / `ops flush`：连不上服务器（`Error::is_transient`，改动都还在
  队列里）是 3；登录被拒绝也留着改动，但重跑没用、要先改密码，所以是 1；服务器拒绝了改动或草稿是 1。
- `send` / `outbox retry` / `mark-sent` / `outbox flush`：`queued` / `sending` 是 3，`failed` 是 1，
  `unknown` 是 4；`accepted`（已发出、只是没存进 Sent）算成功。`outbox flush` 只看这次处理的条目：
  `unknown` 和 `failed` 的等用户处理，不会被它重试，也就不影响它的退出码。
- `sync` 有账户失败是 1：同步本身没有要排队的东西。

## 添加账户

`account add` 先 `check_login`（与 GUI 的添加账户对话框相同），登录成功才写 keyring 和账户行，
拼错的密码不会留下任何东西。`--no-verify` 跳过这一步，用于离线时或脚本里的本地测试服务器；
这时密码错误要到第一次 `sync` 才发现。只验证 IMAP：SMTP 的登录在第一次发信时才检查。

## 写文件

两个命令都不覆盖已有文件：路径是用户给的，但内容来自陌生人，误覆盖不可挽回。`read --save DIR`
遇到重名时加编号（`a (1).pdf`）；`attachment --output PATH` 指定了确切的名字，已存在（包括符号链接，
`create_new` 不跟随）就报错，要覆盖可以写到 stdout 再重定向。
