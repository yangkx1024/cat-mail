# 建草稿时就带上附件

- 日期：2026-10-23
- 类型：重构

## 背景

Issue #112。#110 之后 `reply` / `forward` 的正文和收件人在建草稿时就写进去，但 CLI 的 `--attach`
仍是先建草稿，再逐个 `attach_file`（`finish_draft`）：每个附件多一个草稿版本，`catmail watch`
里后台同步在两次之间跑过的话还多一次上传到 Drafts；附件出错时 CLI 还得删掉刚建的草稿。

## 决策

- `MailClient::create_draft` / `reply` / `forward` 都加一个 `files: Vec<PathBuf>` 参数，而不是放进
  `Forwarding`：三个入口一致，GUI 和测试传 `Vec::new()`。
- 上限照 `attach_file` 算，但草稿还不存在，按它将用的身份查（新增 `Drafts::new_size_limits`，
  身份解析和 `insert` 相同）。转发带来的原信附件计入已用额度，和之后再 `attach_file` 的结果一致。
- 没指定身份时，查上限时解析出的默认身份就是草稿的身份，传给 `insert` 的是这个确定的 id：复制期间
  默认身份可能被改，`insert` 再解析一次会把附件放到没按它检查过的身份上（review 发现，20 MiB 的
  附件进了 SIZE 1 MiB 的身份）。没选在事务里复查合计：还要找出是哪个文件超的，代码更多。
- 写入事务里不再复查合计：新草稿在提交前没有别人能往里加附件（`attach_file` 的复查是为这种并发）。
- 文件出错包成新的 `Error::Attach { path, error }`：一次给多个文件，调用方要知道是哪个；`kind()` 跟随
  内层错误。已有草稿的 `attach_file` 不包，调用方本来就知道路径。

## 方案

- `catmail-core/src/drafts.rs`：从 `attach_file` 抽出 `copy_file`（检查是文件、复制进 BlobStore、
  定 MIME），新增 `copy_files`；`insert` 在有文件时先复制，再和草稿、转发的附件在同一个事务里写入。
- `catmail-store/src/drafts.rs`：`new_size_limits`。
- `catmail-cli/src/cmd/compose.rs`：`--attach` 直接交给 core，`finish_draft` 不再 `attach_file`，
  也不再在失败时删草稿。
- 文档：`docs/architecture/sending.md` 的附件一节。
- 测试：`send.rs::files_given_with_a_new_draft_come_with_it_or_no_draft_is_made`（合计超限、文件不存在
  都不建草稿且错误带路径）；`identities.rs` 按所选身份的 SIZE 限制，以及
  `files_are_checked_for_the_identity_the_draft_is_made_as`（另一个任务不停切换默认身份，没有修复时
  稳定失败）；`drafts.rs` 的回复、转发带文件。
  CLI 的 `failed_command_leaves_no_draft` 照旧通过。
