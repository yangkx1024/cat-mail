# 发件身份可以改名

- 日期：2026-11-01
- 类型：功能

## 背景

Issue #185：添加账户后需要能修改账户名称。发信时 From 里的显示名来自发件身份（`identities.display_name`），
添加账户时填的名字只复制给第一个身份，之后没有任何地方能改；默认身份又不能删，所以随账户建出的那个名字
根本改不了。

## 决策

- "账户名称"按发件人名称（From 的显示名）处理，不做账户的备注名（侧边栏里代替邮箱地址的"工作"、"个人"）。
- 去掉 `accounts.display_name` 和 `Account::display_name`，名字只在身份上。它原本只在建账户时复制给第一个
  身份，和 GUI 加身份时预填名字；留着就要在改默认身份名字时同步，但"同步哪一个身份"（随账户建出的还是现在的
  默认）说不清，两份数据也容易不一致。预填改用默认身份的名字，CLI JSON 账户的 `display_name` 也取默认身份的
  （和 `smtp` 字段一样），脚本看到的结构不变。`NewAccount::display_name` 保留，作为第一个身份的名字。
- 改名后用这个身份写的草稿 `revision` 加一，让 Drafts 里的副本换成新名字；不改 `updated_at`，用户没动这些
  草稿，列表顺序不变。outbox 里的信是入队时冻结的快照，照原样发出（改快照等于重写用户已发出的信）。名字没变时
  什么也不做。
- 名字规范化（去 NUL、去首尾空白、空白即 `None`）放在 core 的 `clean_name`，改名、`add_account`、
  `add_identity` 都用它：以前 CLI `--name ""` 会存下空字符串。

## 方案

- store：schema 去掉 `accounts.display_name`；`Identities::set_display_name` 返回账户和重新上传的草稿数，
  草稿经 `Drafts::reupload_identity` 只加 `revision`。
- core：`MailClient::set_identity_name(id, Option<&str>)`，提交后发 `AccountChanged`，有草稿时再
  `drafts::changed`（发 `DraftsChanged` 并唤醒后台同步上传）。
- CLI：`catmail identity name <id> [<name>]`。
- GUI：Settings › Accounts 的身份行显示成"名字 <地址>"（和写信窗口的 From 一致），每个身份（包括默认的）
  有"Rename"按钮，打开只有名字一栏的对话框，存好才关、失败留在对话框里。它和文件夹的命名对话框原本几乎是
  同一份代码（尤其是"打开后才聚焦"的顺序），就把 `FolderForm` 抽成 `submission::NameDialog`，两处共用；
  元素 id 由调用方给（`NameIds`），文件夹原有的 id 不变。
- 文档：`sending.md`（发件身份 / 名字）、`cli.md`、`gui.md`、README。
- 测试：store `renaming_one_uploads_its_drafts_again`；core `tests/identities.rs`
  `renaming_an_identity_replaces_its_copies_in_drafts`（假服务器：Drafts 副本换成新名字，outbox 里已排队的照
  原样发出，空白清掉名字，不存在的身份报错）；CLI `name_is_set_and_taken_off`；GUI
  `renames_a_sending_address`（Enter 和 Save 按钮各提交一次，第二次输入替换选中的旧名字）。
