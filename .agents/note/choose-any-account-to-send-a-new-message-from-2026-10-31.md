# 新邮件可以选任一账户发信

- 日期：2026-10-31
- 类型：功能

## 背景

Issue #175：有多个可发信账户时，写信窗口的 From 只能在草稿所属账户的身份里选。草稿在建出时就绑定了
账户（GUI 只读 `list_identities(draft.account_id)`，`Drafts::update` 拒绝别的账户的身份），所以两个账户、
各只有默认身份时 From 根本不能选，新邮件的发信账户被锁死在当前选中的账户上。

## 决策

- 在引擎里让草稿换账户，而不是在 GUI 里新建一封草稿、复制内容再删旧的：后者草稿 id 会变（窗口按
  草稿 id 记），附件要重新复制，Drafts 副本也要另做处理。
- 范围只到新邮件（和转发）。回复保持在原信的账户：发出后要在原信所在账户加 `\Answered`，
  `reply_to_message_id` 只引用本账户的邮件，跨账户会破坏"账户自有的引用"这条规则。回复的判断是
  `reply_to_message_id` 或 `in_reply_to` 不为空。
- 哪些身份可选由引擎决定（`MailClient::draft_senders`），GUI 不自己拼账户和身份。
- 换账户时不重新检查附件上限，发送时的 `SIZE` 检查兜底。
- CLI 的 `draft edit --from` 也在 `draft_senders` 里找，两个前端用引擎的同一套规则；地址先在草稿自己的
  账户里找（不会因为别的账户有同一个地址就把草稿搬走），再依次找其他账户，都用 `identity_by_address`，
  不另写一遍按地址找身份。`compose`、`reply`、`forward` 不变：建草稿时就选账户。

## 方案

- `catmail-store/src/drafts.rs`：`Drafts::update` 遇到别的账户的身份时调用 `move_to`：拒绝回复；把旧副本转成
  没有草稿的上传（`orphan_copy`），未结算上传的 `draft_id` 置空，改 `account_id`、`identity_id`，清掉
  `remote_*` 和 `upload_error`。`update` 改为返回草稿原来的账户。`uploaded` / `upload_refused` 改用
  `end_upload`（`DELETE … RETURNING draft_id`）按行当时的 `draft_id` 结算，否则搬走期间结束的上传会把旧账户的
  文件夹记到草稿上。`DraftUpload::draft` 因此不再有人读（读出后会过时），去掉了。`begin_upload` 加了草稿仍属于目标文件夹账户的
  条件：旧账户的一轮上传在列出草稿后、读快照前遇到搬走时，读到的 revision 是搬走后的，原来会违反
  `(folder_id, account_id)` 外键、让整轮同步失败（review 指出，store 测试
  `a_draft_moved_during_its_old_accounts_upload_pass_is_skipped` 复现），现在跳过，由新账户上传。
- `catmail-store/src/identities.rs`：`Identities::of_draft`，按账户、默认在前列出草稿可选的身份。
- `catmail-core`：`drafts::update` 搬走时对旧账户 `draft_gone` + `changed`；新增 `MailClient::draft_senders`。
- `catmail-gui/src/composer/mod.rs`：From 用 `draft_senders`，订阅任一账户的 `AccountChanged` /
  `AccountAdded` / `AccountRemoved`；选中的身份被删时换成引擎存下的草稿身份，不在 GUI 里另推默认身份。
- `catmail-cli`：`identity::resolve_draft_from`，`draft edit` 用它；草稿搬走时也保存旧账户，删掉那里的副本。
- 文档：`docs/architecture/sending.md`（发件身份）、`docs/architecture/gui.md`（From）、
  `docs/architecture/cli.md`（`--from`）。
- 测试：store 的 `a_draft_moved_to_another_account_leaves_nothing_behind`、`a_reply_stays_in_its_account`、
  `a_draft_may_be_sent_as_any_accounts_identity`；core 的 fake-server 测试
  `a_draft_sent_from_another_account_moves_there`（副本从 A 的 Drafts 删除、出现在 B 的 Drafts）；GUI 的
  `a_new_message_goes_from_any_account`（去掉修复会失败）；CLI 的
  `draft_edit_from_another_accounts_address_moves_the_draft`。回复不能换账户在 store 和 CLI 测。core 测试
  加了 `Env::add_fake_account`（另一个账户在自己的 fake 服务器上），`drafts.rs` 和 `gmail.rs` 共用。
