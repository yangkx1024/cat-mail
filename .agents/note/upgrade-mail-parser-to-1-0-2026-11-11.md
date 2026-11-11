# 升级 mail-parser 到 1.0

- 日期：2026-11-11
- 类型：重构

## 背景

#293。mail-parser 0.11.9 → 1.0.1。1.0 是解析器的重写，API 不兼容：正文访问时才解码，正文 / 附件列表从 id
列表变成迭代器，部分 id 全局编号，正文列表严格按 RFC 8621 `parseStructure`。1.0 修好了 #280 发现的
`<style type="text/css">` 识别问题。1.0 要求 Rust 1.98。

## 决策

- MSRV 1.97 → 1.98。不用改 flake：锁定的 nixpkgs（`fe13242`）已是 rustc 1.98，CI 用 stable。新依赖
  `encodify`、`simdutf8`、`memchr`、`hashify` 都是 MIT / Apache-2.0，`cargo deny` 通过。
- **两份正文里都有的媒体照样列出。** 1.0 按 RFC 8621，不再把两份正文里都出现的图片、音视频算作附件（比如
  `multipart/mixed` 里夹在文字之间的图片）。只取 `attachments()` 的话，这种图片既不内嵌也不在附件里，整个
  消失。`extract` 改为列出 `is_attachment()` 或角色是 `MediaBody` 的部分，内嵌仍由 `is_inline` 判断。
- **传输编码坏了尽量解码。** 0.11 遇到一处不合法（QP 的 `==`、base64 里的杂字节）就整个部分不解码，
  `decode_transfer` 也保留原字节。改用 1.0 的做法：base64 跳过字母表以外的字节，QP 不合法的转义原样保留。
  这是 RFC 2045 §6.7、§6.8 的要求，RFC 8621 也是"尽量解码"，Thunderbird（`mimeenc.cpp`）、mutt
  （`handler.c`）的源码也这样做。原字节不论正文还是附件都没用：正文显示成一屏 `=XX`，附件存下的是 base64
  文本。不认识的传输编码仍原样给出（RFC 2045 §6.4）。解码问题不另外记录，也不在界面上提示。
- HTML 转文本仍用清洗后的 HTML。1.0 从原文转也不会再漏样式表，但清洗后的才是读信区显示的文字
  （`<title>` 已去掉）。
- 空 `Subject:` 1.0 给 `Some("")`，摘要仍当没有主题（`None`），和已存的行一致。其他表头的变化（RFC 2047
  编码字和普通文本之间不再插空格等）不重建已存的摘要（和用户确认过），只影响之后同步的邮件。
- `CONTENT_VERSION` 3 → 4：本机有 26 封的文本变了（见下），解码规则也变了。指纹语料加入一封 QP 不合法的邮件。
- `text/plain` 之后带名字的 `text/html` 备选按 RFC 8621 算附件，HTML 正文由文本转成。本机邮件里没有这种情况，
  照 RFC 办。

## 方案

- `catmail-proto/src/mime/`：`content.rs`（`extract` 用 `decoded()` 取每个部分的数据，不再从原文切片，
  去掉 `raw` 参数；`is_listed`；`decode_transfer` 用 `Encoding::decode`；`text`、`sections`、`filename`、
  `text_charsets` 改用视图 API），`structure.rs`（`body_text`），`mod.rs`（地址用 `AddressList::mailboxes`，
  `TextList`，`Header::offset_*()`）。`catmail-testkit/src/mime.rs` 的 BODYSTRUCTURE 和 section 按
  `children()` / `nested()` / `raw_body()` 重写。core 的集成测试只改 API。
- 顺带清理：按部分下载时 `sections()` 只算一次（`extract`、`body_text` 接收它）；文本部分是标记时 HTML 只转
  一次文本（`OnceCell`，仍然只在需要判断时才转）；`content_id()` 在 1.0 已去掉尖括号，不再自己去。重新解析本机
  邮件，输出与清理前逐字节相同。
- 文档：`docs/architecture/bodies.md` 的"正文的组成"：文本转换的理由、列出哪些部分、传输编码。
- 测试：`media_shown_in_both_bodies_are_listed`（只取 `attachments()` 时失败）、
  `decodes_malformed_transfer_encodings_as_far_as_they_go`（在 0.11 的代码上失败，base64 原样返回）。
  macOS 上 `cargo fmt`、`clippy -D warnings`、全部测试、`cargo deny` 都通过（本机没有 Nix，用的系统 cargo
  1.99.1）。Dovecot 测试只在 Linux CI 上跑。
- 手动：用新旧两版各解析一遍本机 3886 封有原文的邮件（数据库的只读拷贝）。HTML、部分列表（section、类型、
  文件名、内嵌、大小、数据）全部一致。正文选哪些 section（`text_sections` 的规则）也全部一致。26 封文本
  变化，都来自 1.0 的 `html_to_text` 多保留了换行：`Chapel MewsHoveBN3 1AR` 分成几行，`WHAT'S NEXTKindly`
  断开，个别地方多一个空行；摘要只有 2 封变化。
