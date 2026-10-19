# GUI 登录页：在窗口里添加账户

- 日期：2026-10-19
- 类型：功能

## 背景

最小读信窗口只能读 `catmail account add` 添加的账户，而窗口运行时 CLI 打不开同一个数据
目录，手动测试要来回关窗口。用户要求加一个简单的登录页，直接在窗口里登录。

## 决策

- **先登录再保存**：新增 `MailClient::check_login(&NewAccount, &str)`，连接 IMAP、登录、
  LOGOUT，不写数据库也不写密钥环；成功后才 `add_account` 并同步。考虑过"先 `add_account`
  再同步，失败就删除账户"：删除路径更长，中途崩溃会留下账户行，而且把"登录失败"和"之后
  同步失败"混在一起。只检查 IMAP：SMTP 要到 M3 才有。
- **凭据构造放在 `auth.rs`**（`new_account_credentials`，按 `AuthKind` 匹配），与已存账户的
  `credentials` 并列，M6 加 OAuth2 时仍只改这一个文件。
- **表单字段与 CLI 一致**：邮箱、密码、名称（可选）、用户名（默认等于邮箱）、IMAP / SMTP 的
  服务器、端口（空则按加密方式取默认值，占位文字随之变化）、加密方式（TLS / STARTTLS / None，
  选 None 时提示只用于本地测试服务器）。没做按域名猜服务器：猜错比空着更难排查。
- **出现方式**：没有账户时自动显示且不能取消；侧栏底部"Add account…"随时打开（可取消）。
  登录页整个替换三栏，而不是对话框：表单较长，对话框里还要再滚动。
- 表单只检查端口格式（引擎拿到的是数字，没法替它解析），其余由 `check_login` 里与
  `add_account` 相同的 `validate_new_account` 和服务器的回应决定，两处不会不一致；错误显示在
  按钮上方。回车在任一字段里都会提交。

- **codex review 修复**：`check_login` 成功、`add_account` 开始后点"Cancel"会丢弃表单的任务，
  但引擎的阻塞任务照样把账户和密码存下，新账户还错过了首次同步。登录进行中"Cancel"不可用。
  测试 `cancel_is_off_once_signing_in_has_started`（gpui-kit 的 Button 不通过无障碍属性报告
  disabled，所以测试点击"Cancel"并断言表单仍在、账户最终加上）。

## 方案

- `catmail-core`：`client.rs` 的 `check_login`，`auth.rs` 的 `new_account_credentials`。
- `catmail-gui`：`add_account.rs`（表单、`Values::new_account` 纯函数、`AddAccountEvent`）；
  `mailbox.rs` 在账户为空时打开登录页，`Added` 后关闭并同步该账户（同步计数改为可重入的
  计数器，手动同步与新账户的同步可以并存）；侧栏加"Add account…"。
- 文档：README 的图形界面用法、架构文档"GUI"一节。

验证：`cargo fmt`、`clippy -D warnings`、全部测试通过。新增测试：core 的
`check_login_logs_in_and_stores_nothing`（假服务器：正确密码登录并 LOGOUT、错误密码
`AuthFailed`、空密码 `InvalidInput`，账户数不变）；GUI 的表单取值 3 个单测（默认值、填写值、非法端口），以及 UI 集成测试
`signs_in_from_the_window`：在真实窗口里填表、选 None，先用错误密码（显示错误、没有存账户），
再改对密码按回车，登录页关闭、收件箱出现 6 封邮件。变异测试：去掉 `check_login` 调用后该测试
失败。截图工具加了登录页的截图。
