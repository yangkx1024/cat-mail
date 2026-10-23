# 发件身份：经各自的 SMTP 服务器发信

- 日期：2026-10-23
- 类型：功能

## 背景

Issue #80：IMAP 和 SMTP 共用一个密码，也只有一台 SMTP 服务器，没法经 Resend 这类只提供 SMTP 的服务
（用户名 `resend`、密码是 API key）发信。需求是一个账户可以有多个 SMTP 凭据，写信时选用哪个发
（send as）。顺带：添加账户时的连接测试（`check_login`）只登录 IMAP，SMTP 填错要到发信才发现。

## 决策

- 账户只是 IMAP 邮箱，发信全部经"发件身份"（`identities` 表，`Identity` / `NewIdentity`）：地址、显示名、
  用户名、SMTP 服务器。添加账户时用 `NewAccount::smtp` 建出第一个身份，它没有自己的 keyring 条目，用账户
  的密码；之后添加的各有一个。
- 每个账户恰好一个默认身份，新草稿从它发，可以换，不能删。起初的做法是账户自带的 SMTP 留在 `accounts`
  上、身份为空（`None`）表示它：改动小，但每个和发件人打交道的地方都分两支（解析发件人、记 SIZE、连接
  状态、"账户地址排第一"在 core / CLI / GUI 各写一遍），也做不到"平时都走 Resend"。还没发布、schema 还能
  原地改，就换成了现在的模型。
- 不做"只发信、没有 IMAP 的账户"：同步、Drafts、Sent 都假设有 IMAP，改动大，用户也不需要。
- `drafts.identity_id`、`outbox.identity_id` 必填；输入的 `DraftContent::identity` 仍是 `Option`，`None`
  存成默认身份，免得每个调用方先查默认身份。身份属于草稿的账户由 repository 检查，复合外键兜底。删除身份
  时草稿改到默认身份；outbox 里还有经它发的信时拒绝删除，否则那些信没有服务器可用（考虑过级联删除
  outbox，会悄悄丢掉要发的信）。
- 一轮发送每个身份各开一条连接，连接失败、退避和账户状态也按身份（`AccountStatus::send`）：一台服务器
  登录不了不该挡住另一台的信。`FlushReport::errors` 同样按身份（原来是一个 `error`，取最后一次失败）：
  否则 A 的密码被拒绝、B 暂时连不上时 `outbox flush` 报 3（重试即可）而不是 1，CLI 也按身份判断。
- 回复默认用原信寄给的那个地址，几个身份地址相同时默认的优先；都不是时用默认身份。
- 同一地址、同一服务器和用户名的身份只能有一个；同一地址走两台服务器可以（GUI 在下拉框里附上服务器）。
- `check_login` 也登录 SMTP，失败包在 `Error::OutgoingServer` 里，前端能说清是哪台服务器；新增
  `check_identity_login`，以及先检查再添加的 `add_account_checked` / `add_identity_checked`（前端都用它们，
  见同日的附件上限）。代价是 SMTP 端口被网络挡住时账户加不上（CLI 可 `--no-verify`）。
- CLI 的 `--json` 账户对象保留 `smtp` 字段，取默认身份的服务器，脚本看到的结构不变。

## 方案

- store：`identities` 表（`is_default` 加部分唯一索引，`secret_key` 可为空）与 `Identities` repository
  （`default_of`、`set_default`；`sending` 一次查出发信要的身份、SIZE 和密钥键——自己的或账户的）；`Accounts::insert` 同时建默认身份；
  `accounts` 去掉 SMTP 列；`drafts.identity_id` 用 `coalesce` 补成默认身份；删除账户时同一事务里读出身份
  自己的密钥键（`secret_keys`），删行后一并删密钥；删除身份时草稿经 `Drafts::reassign_identity` 改到默认
  身份。
- core：`connection::sender` 解析发件人（地址、服务器、登录、SIZE），`connect_smtp` 按身份连；`send.rs`
  每个身份一条连接；`draft_sync` 上传的副本也用身份的 From；`MailClient` 新增 `list_identities`、
  `check_identity_login`、`add_identity`、`add_identity_checked`、`set_default_identity`、
  `remove_identity`，事件 `IdentitiesChanged`，`AccountOverview::identities`；先写密钥再插行、失败删密钥的
  步骤收进 `insert_with_secret`，账户和身份共用。`Account` 没有 `smtp` 了。
- CLI：`catmail identity add/list/default/rm`，`compose`、`reply`、`forward`、`draft edit` 的 `--from`；
  JSON 加 `identity`、`is_default` 字段（只增字段，schema 版本不变）。`--smtp-*` 和密码选项由
  `account add` 与 `identity add` 共用。
- GUI：写信窗口有不止一个身份时 From 是下拉框；窗口自己订阅 `IdentitiesChanged`，身份变了（加、换默认、
  删）时跟着更新，被删的换成默认身份。根子在 store：`Drafts::update` 收到一个已删除的身份时保留草稿现在的
  （删除时已改成默认身份，id 不重用），所以窗口在收到事件之前保存也不会失败；Settings › Accounts 在账户下列出身份，默认的标着
  "Default"，其余可"Make default"或删除，"Add sending address"在对话框里原地换成表单。设置对话框
  的 `on_ok` 改成不关闭：它没有要确认的东西，原来字段里按 Enter 会关掉设置、丢下正在添加的身份（GUI
  测试发现）。登录页的字段和"任一字段按 Enter 提交"抽到 `login_form.rs`（`LoginFields`、
  `ServerFields`、`submit_on_enter`），两个表单共用。
- 文档：`docs/architecture/sending.md`（发件身份）、`storage.md`、`cli.md`、`gui.md`、README。
- 测试：store 的 identities 单测（建账户即有默认身份、重复、跨账户、默认身份可换不可删、删除时草稿改到
  默认 / outbox 拒绝、随账户删除）；core `tests/identities.rs`（两台假 SMTP 一轮发出、Drafts 副本的
  From、一台登录失败只挡自己的信、删除、换默认后新信从它发、跨账户拒绝、`check_identity_login`），
  `check_login` 测 SMTP 失败，回复选身份的单测，身份密码的生命周期；CLI 的 `identity add`、`--from`；
  GUI 的添加身份、设为默认、删除旧的，From 下拉框，以及写信窗口开着时它的身份被换掉；CLI 一个身份要
  先改、另一个暂时连不上时 `outbox flush` 退出码是 1。
