# CLI 添加账户先验证登录，`--json` 显式结构，区分退出码

- 日期：2026-10-23
- 类型：修复

## 背景

- #37：`catmail account add` 直接 `add_account`，拼错的密码会留下账户行和 keyring 条目，要到第一次
  `sync` 才发现，还得 `account remove` 再来。GUI 早就先 `check_login`。
- #38：`--json` 直接序列化 `catmail-types`（`flags` 是位掩码，types 改字段名会悄悄改输出，`ops`
  的回放又是手拼的 `json!`）；退出码只有 0 / 1 / 2，脚本分不出"还在排队"和"可能已送达"；
  `attachment --output` 会覆盖（并跟随符号链接），而 `read --save` 永不覆盖；`main.rs` 和
  `compose.rs` 各七百多行，混着 clap 类型、命令体和表格打印。

## 决策

- `account add` 默认先 `check_login`，加 `--no-verify` 跳过（离线、本地假服务器）。只验证 IMAP，
  与 GUI 相同。
- JSON 由 `json.rs` 里的专用结构输出，转换时对枚举穷尽匹配：types 加变体会编译失败，而不是悄悄
  多出一个值。每个文档是带 `schema_version` 的对象，列表放在键下；时间用 RFC 3339（比 unix 秒
  好读，jiff 已是依赖）；字段名按 CLI 的词汇（`changes_refused`、`account`），不跟内部的 `ops_failed`
  / `account_id`。快照测试用 `serde_json::json!` 字面量比较，没有为此引入 insta。
- 退出码：3 = 没做完但已保留（排队等下次），4 = 可能已送达。服务器拒绝等仍是 1。一次命令有多种
  结果时取 4 > 1 > 3，保证 3 只意味着"稍后重跑即可"。改动回放用 core 的 `Error::is_transient` 判断：
  连不上服务器是 3，登录被拒绝是 1（改动虽然还在，重跑没用）；`sync` 失败仍是 1，同步没有排队的东西。考虑过给"服务器拒绝"单独一个码，但它和
  其他失败一样需要人处理，脚本没有不同的做法，没加。
- `attachment --output` 改为 `create_new`：已存在（含符号链接）就报错，不像 `--save` 那样加编号，
  因为用户给的是确切文件名；要覆盖可以写 stdout 再重定向。
- 按 issue 建议拆分：`cmd/` 每组命令一个文件，打印统一进 `output.rs`。

## 方案

- `crates/catmail-cli/src/cmd/`：`account`（`--no-verify`、先 `check_login`）、`sync`、`search`、
  `read`、`ops`、`compose`、`outbox`（从 `compose.rs` 分出）、`watch`、`gc`；`mod.rs` 放共用查找。
- `json.rs`：所有 `--json` 输出与快照测试；`exit.rs`：`Outcome` / `Exit` 错误，`main` 用它决定退出码。
- `files::write_new_file`；`output.rs` 收齐各命令的表格与视图。
- 文档：新增 `docs/architecture/cli.md`（结构、JSON 约定、退出码、写文件），README 补退出码和
  `--no-verify`。
- 测试：`account::wrong_password_adds_nothing`（假 IMAP 拒绝登录时既无账户也从未写过 secret；
  `--no-verify` 照旧添加）、`outbox::exit_code_says_how_a_send_ended`（假 SMTP 丢确认 → 4、
  收件人 550 → 1、服务不可用 → 3）、`json` 两个快照测试、`exit::code_survives_context`、
  `files::never_follows_links`，`ops` 测试补充离线时退出码为 3、未知文件夹为 1。
