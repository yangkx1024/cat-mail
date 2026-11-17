# 地址栏和回复/转发的内容拆出 drafts.rs

- 日期：2026-11-17
- 类型：重构
- Issue：#320（第 2 项）

## 背景

`catmail-core/src/drafts.rs` 约 1.7k 行，里面有两组不依赖引擎的纯函数，占了这个文件几乎全部单元测试：
地址栏的解析、显示与检查，以及回复/转发一开始的收件人、主题和引文。它们和草稿的存取、附件、组信混在一起，
读草稿的流程时要跳过它们。

## 决策

- 按 `lib.rs` 现有的平铺风格，建两个同级模块，而不是 `drafts/` 下的子模块：
  - `addresses.rs`：`parse_addresses`、`format_addresses`、`split_entries`、`parse_entry`、`validate`、
    `check_address`。
  - `answer.rs`：`Own`、`reply_content`、`ReplyRecipients`、`reply_recipients`、`reply_subject`、
    `forward_subject`、`prefixed`、`quote`、`attribution`、`forward_content`、`eml_name`。
- 不放进 `catmail-types`：`check_address` 要用 `catmail_proto::smtp::is_envelope_address`，types 不依赖 proto。
- 公开路径不变：`catmail_core::parse_addresses`、`format_addresses` 改从 `addresses` 再导出。原来在
  `drafts` 里私有、现在 `drafts` 要跨模块用的（`Own`、`reply_content`、`reply_recipients` 和
  `ReplyRecipients::cc`、`forward_subject`、`forward_content`、`eml_name`）提到 `pub(crate)`，其余仍是私有；
  `validate`、`check_address` 本来就是 `pub(crate)`。
- `drafts.rs` 不留转发 `check_address` 的 `use`：调用方直接改路径。`client.rs` 正由别的改动拆分，只改了
  `validate_new_identity` 里的一行（写成 `crate::addresses::check_address`，不动导入列表，免得冲突）。
- 行为不变，只搬代码；`invalid_text`（把 `validate` 的错误放进写事务）留在 `drafts.rs`，它属于 `update`。

## 方案

- 新文件 `crates/catmail-core/src/addresses.rs`、`crates/catmail-core/src/answer.rs`，各自带着原来的测试
  （地址 3 个，回复/转发 7 个；测试用的 `addr` 两边各有一份）。
- `drafts.rs` 改为从这两个模块导入，去掉不再用的导入；`lib.rs` 声明模块、调整再导出；`send.rs` 的
  `check_address`、`validate` 改走 `addresses::`；`client.rs` 一行。
- `docs/architecture/` 只按名字提到 `parse_addresses` / `format_addresses`，没写位置，不用改。
- 验证：`cargo fmt --all`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`
  通过；catmail-core 的单元测试前后都是 77 个，只有那 10 个从 `drafts::tests` 换到了新模块下。
