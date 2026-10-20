# MIME 构建器拒绝放不进尖括号的地址和 Message-ID

- 日期：2026-10-20
- 类型：修复

## 背景

Issue #22（纵深防御）。`build_message` 对主题、显示名、附件名做了 `one_line`，对 In-Reply-To /
References 做了 `fits_in_angle_brackets`，但地址的 `email` 和本信的 `message_id` 原样交给
mail-builder。mail-builder 1.0.0 会写出 `From: <a\rBcc: x@evil.example>`，SMTP 的 `DotStuffer`
再把裸 CR 规整成 CRLF，收件 MTA 就看到注入的 `Bcc:` 头。今天 core 在存草稿和发信前都校验了地址，
Message-ID 也是 `uuid@域名`，不可利用；但 proto 的 API 对以后跳过这些校验的调用者没有保护。

## 决策

- 地址和 Message-ID 用与 threading ID 相同的 `fits_in_angle_brackets`（非空、可见 ASCII、不含
  `<>`）。threading ID 可以悄悄丢掉，地址和本信的 Message-ID 丢不得，所以不合规就返回错误：
  `build_message` 改为返回 `Result<Vec<u8>, InvalidHeader>`，core 映射成 `InvalidInput`。
- 没有改用更严格的 `is_envelope_address`（要求含 `@`）：这里防的是头注入，信封规则留给 SMTP 和
  core 的 `check_address`。
- 附件名仍不在 proto 清洗（core 读写两端都经 `safe_filename`），只在 `Part.filename` 的文档里写明
  仅供显示、落盘前必须清洗。

## 方案

- `crates/catmail-proto/src/mime/build.rs`：新增 `InvalidHeader`；Message-ID 和每个地址在交给
  mail-builder 的地方经 `bracketed` 检查（`address()` 返回 `Result`，以后加的地址字段也绕不过）；
  新测试 `addresses_and_message_id_cant_inject_headers`。
- `crates/catmail-proto/src/mime/content.rs`：`Part.filename` 文档。
- `crates/catmail-core/src/drafts.rs`：`build` 把错误映射成 `InvalidInput`；发信和草稿上传原有的
  错误处理照旧适用（上传失败记到草稿上，不影响其它草稿）。
- `docs/architecture/sending.md`：补充构建时的检查。
