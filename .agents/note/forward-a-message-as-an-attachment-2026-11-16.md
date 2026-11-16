# 作为附件转发（message/rfc822）

- 日期：2026-11-16
- 类型：功能

## 背景

#312。转发原来只有内联一种：原信作为引文、附件复制进草稿。这会丢掉原信完整的头部（Received、DKIM 签名、
Message-ID 等）和 MIME 结构。把可疑邮件转给管理员、转交要核对原件的邮件时，要的恰恰是这些。Thunderbird、Outlook
都有"作为附件转发"。

## 决策

- `Forwarding::as_attachment`：原信整封作为 `message/rfc822` 附件，文件名为主题加 `.eml`，没有引文，正文只是用户
  写的文字。原文走 `body::raw_message` 那条路（新增 `body::raw_blob`，直接引用 blob，不读进内存）。
- 原信计入附件上限：先按 `RFC822.SIZE` 检查，放不下就报错、不下载；下载后再按实际大小查一次。
- **编码**：mail-builder 把所有非 `text/*` 的部分 base64，`message/rfc822` 也不例外，违反 RFC 2046 §5.2.1（只允许
  7bit / 8bit / binary）。现在原信能原样走就原样放：纯 ASCII 用 `7bit`，有 8 位内容且去处收得下时用 `8bit`。
  Drafts 的副本走 IMAP，总是收得下；SMTP 服务器要公布了 8BITMIME 才行。服务器没公布 8BITMIME，或原信有 NUL、
  孤立 CR / LF、超过 998 字节的行时，退回 base64。这违反 RFC，但 Outlook / Exchange 一直这样发，主流客户端都读得
  出来。
- 放弃的方案：
  - 没有 8BITMIME 就拒发：对用户不友好。
  - 逐层把原信重新编码成 7-bit：复杂，还会破坏 DKIM 签名，而保留原样正是作为附件转发的意义。
- 邮件在入队时构建、冻结（"所确认的就是所发的"），发送时才知道服务器能力就来不及了。所以 8BITMIME 和 SIZE 一起
  记在身份上：新增 `identities.smtp_8bitmime`，store 里是 `SmtpAnnounced`，取代单独的 `smtp_max_size`。发信连接
  时记，添加账户、身份的登录检查时也记。还没连过的当作公布了：几乎所有提交服务器都公布。服务器不再公布后的
  第一封仍按 8bit 发，之后入队的才改成 base64。
- GUI：Forward 仍是一次点击，旁边加一个窄的下拉箭头（`buttons::menu_button`），菜单里有 Forward 和
  Forward as attachment，与列表勾选框旁的菜单同一种做法。CLI 加 `catmail forward --as-attachment`，MCP 的
  `forward` 加 `as_attachment` 参数。

## 方案

- 引擎：`catmail-types` 的 `Forwarding::as_attachment`；`drafts::forward_attached`、`eml_name`；
  `body::raw_blob`；`drafts::build` 用 `Destination`（Drafts / Smtp）代替 `with_bcc`，Smtp 时读身份记下的 8BITMIME。
- proto：`Outgoing::eight_bit`、`build::attached_message`；`smtp::Session::eight_bit_mime`。
- store：`identities.smtp_8bitmime`、`SmtpAnnounced`、`set_smtp_announced`。
- testkit：假 SMTP 的 `no_8bitmime`。
- 测试：
  - `build` 单元测试覆盖 7bit / 8bit / base64 回退，并用 mail-parser 读回嵌套的邮件。
  - `drafts` 集成测试：原文逐字节进入 Drafts 副本；超过附件上限时报错，且不发 FETCH。
  - `send` 集成测试：8bit 原样发出；服务器不再公布 8BITMIME 后先按 8bit 发一封，之后改为 base64，收件方能解码出原信。
  - CLI 测试，以及 MCP 的预算测试。
- GUI 没有在运行的应用里看过：本机跑不起 nsh 的界面检查。
