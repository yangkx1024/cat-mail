# 按文本字符集读原始 8 位字节的邮件头

- 日期：2026-10-26
- 类型：修复

## 背景

#131：换成 imap-proto 之后，BODYSTRUCTURE 里的字符串按 UTF-8 有损转换。附件文件名直接用原始 8 位字节写
（如 GBK，不按 RFC 2231 / 2047 编码）、邮件又大到按部分下载时，文件名变成 `U+FFFD`。

动手时发现 issue 和 `protocol.md` 里"整封下载不受影响"不对：mail-parser 解析邮件头也按 UTF-8 有损读，整封
下载的同一封邮件文件名一样是 `U+FFFD`；同步时的主题和显示名也是（`mime/mod.rs` 原有的测试只检查了地址）。所以
只按 issue 的建议取 `.MIME` 原始头不够，还要知道原始字节是哪个字符集。用户决定主题和显示名在这里一起修。

## 决策

- 字符集按其他客户端的做法猜：同一封邮件里所有文本部分（按 MIME 顺序）的 `charset`，解析好的邮件和
  BODYSTRUCTURE 用同一条规则；附件先看这个部分自己的。UTF-8 和 ASCII 不算线索（字段不是 UTF-8；ASCII 文本说明不了发件人用
  什么字符集，它的标签按 WHATWG 又指 windows-1252）。没有线索就保持原样。没有引入 chardetng 之类的字符集
  探测：多一个依赖，而这种邮件几乎都有同字符集的正文。
- 按字段转码（`transcode_fields`）：合法 UTF-8 的字段原样保留，只转不是 UTF-8 的；每个字段用线索里第一个能
  完整解码（encoding_rs 不做替换地解码成功）的字符集。整块头按一种字符集转会弄坏其中的 UTF-8 字段。
- 字符集用 encoding_rs 直接判断和解码（新增直接依赖，版本同 mail-parser 用的）：只用它认识、ASCII 兼容的编码。
  codex 评审指出两处：UTF-16 能把整个字段（连同字段名、冒号、换行）解码成功，毁掉邮件头结构；`ansi_x3.4-1968`
  等 ASCII 别名映射到 windows-1252，什么字节都接受，抢在正文的 GBK 前面。最初只过滤了 `us-ascii` 等字面写法，
  用的是 mail-parser 的 `charset_decoder`，它不说明一个编码是否 ASCII 兼容。
- 先转码再解析，不是先解析再转码：GBK 双字节的第二个字节可能是 `\` 或 `"`，先解析会被当成转义或引号。
- 按部分下载时只为参数值带 `U+FFFD` 的部分另发一条 `BODY.PEEK[<section>.MIME]`，不并进取正文的那条 FETCH：
  那条缺项时会退回整封下载，文件名的问题不值得这样。这条解析不了（`Incomplete`）只是文件名保持原样。
- 同步时摘要邮件头改在 FETCH 的各项合并完之后解析，因为 BODYSTRUCTURE 和 `HEADER.FIELDS` 的顺序不定。
- 同步时只有不是 UTF-8 的邮件头才算线索，绝大多数邮件不多做事。
- `/simplify` 时放弃的方案：把 MIME 头以 `"n.MIME"` 为键放进正文部分那张 map（省掉 `content` 的第二个参数，
  但一张 map 装两种值）。

## 方案

- `catmail-proto/src/mime/mod.rs`：`transcode_fields`、`legacy_encodings`；`parse_headers` 多一个 `hints` 参数。
- `mime/content.rs`：`extract` 多一个按 section 的 MIME 头参数（没有的部分从原文按偏移切；替身邮件的头总是
  合法 UTF-8，不会重读），`filename` 经 `transcode_fields` 重读。
- `mime/structure.rs`：`MimeNode::garbled_sections`、`text_charsets`、`Leaf::all_params`；`content` 多一个
  按 section 的 MIME 头参数。
- `imap/session.rs`：`fetch_mime_headers`，与 `fetch_sections` 共用 `fetch_parts`（空列表不发命令）；
  `merge_fetches` 用 BODYSTRUCTURE 的文本字符集解析邮件头。
- `catmail-core/src/body.rs`：按部分下载时取 garbled sections 的 MIME 头（`Fetched::Parts` 改成装箱的
  `Parts`，clippy 的 `large_enum_variant`）。
- testkit 假服务器支持 `BODY.PEEK[n.MIME]`。
- 文档：`bodies.md` 新增"原始字节的邮件头"，`sync.md` 的邮件头一节指向它，`protocol.md` 去掉"代价"一段。
- 验证：proto 单元测试（整封下载的文件名、第二字节是 `\` 的 GBK 字、无线索保持原样、部分自己的 charset 优先、
  合法 UTF-8 不重读、按部分下载用 MIME 头、按字段转码、线索过滤、不用改动 ASCII 的编码、ASCII 别名不抢先）；`fetch_mime_headers` 的假服务器测试；core 的
  `names_in_raw_bytes_read_the_same_whole_or_in_parts`（两条路径各下载一次）和
  `headers_in_raw_bytes_are_read_in_the_charset_of_the_text`（同步）；Dovecot 测试的大邮件加上原始 GBK 的
  发件人名和附件名。这些 core 测试和 Dovecot 测试去掉修复后都失败。
