# 正文与附件

[← 架构](../architecture.md)

正文按需下载（`MailClient::download_message`），不在同步时预取。所有按需操作经过同一个入口
（`catmail-core/src/body.rs` 的 `on_server`）：从连接池取连接、EXAMINE 比对 UIDVALIDITY、在账户
的取消信号上 select。

## 两条下载路径

按 RFC822.SIZE 分：

- **≤ 2 MiB：整封下载。** `UID FETCH <uid> (BODY.PEEK[])`，原文存入 BlobStore
  （`message_bodies.raw_blob_hash`），解析后附件随即解码入库。绝大多数邮件走这条路：完整原文
  最不依赖服务器 BODYSTRUCTURE 的正确性，也留下了 `.eml`（转发、查看原文、以后换解析器重建）。
- **> 2 MiB：按部分下载。** 先取 BODYSTRUCTURE，只取会作为正文显示的部分（单个不超过 2 MiB）；
  附件只记录 section、传输编码和估算大小，第一次打开时再 `BODY.PEEK[<section>]` 下载、解码、入库
  （`MailClient::attachment_content`）。超过 2 MiB 的正文部分和附件一起列出（`text/plain` 或
  `text/html`，没有文件名），打开时单独下载，内容不会无处可看。
- **退回整封下载**：BODYSTRUCTURE 解析失败，或服务器对要取的正文部分回 NIL、干脆不回。缺了正文
  的结果一旦按"已下载"存下，就再也不会重取。`raw_message` 遇到尚未下载的大邮件时整封下载一次，
  同时存下正文。

## 同一套提取逻辑

按部分下载时，用 BODYSTRUCTURE 和取回的部分**重建一封结构相同的替身邮件**（没取的部分是空的
base64 正文），交给 mail-parser 按同样方式解析。于是两条路径选出的正文部分、字符集与传输编码的
解码、RFC 2047 / 2231 文件名完全一致；"哪些部分会显示"也是在不含数据的替身邮件上用同一套规则
判定的。

- BODYSTRUCTURE 的值来自服务器：写进替身邮件头之前去掉换行，token 只保留合法字符，不能伪造邮件头；
  叶子部分即使自称 `multipart` 也按 `application` 写出、去掉 `boundary`，否则它会吞掉后面的部分、
  打乱 section 编号。
- 替身邮件解析出的部分对不上 BODYSTRUCTURE 的 section 时一律视为未下载。
- IMAP section 编号按 RFC 3501 §6.4.5 从解析结果计算，附加的邮件（message/rfc822）算一个部分。

## 正文的组成

- `text`：所有正文文本部分；只有 HTML 时由 HTML 转换。
- `html`：只在有真正的 HTML 部分时存在，经 `ammonia` 清洗：
  - 去掉脚本、表单、iframe、事件属性和 `javascript:` 链接；
  - 样式表不交给 ammonia（它不看 `<style>` 里面，写回时原样输出原始文本）：分词时按树构建器的规则
    （`script`、`textarea` 等之后切到原始文本，SVG / MathML 里的忽略）取出 `<style>` 的内容，经
    `mime/css.rs` 用 cssparser 的记号逐条重写，合成一个 `<style>` 放在输出开头。只保留普通规则和
    `@media` / `@supports`（最多嵌套三层）；值里有 `url()`、`image()`、`image-set()`、`cross-fade()`、
    `src()`、`element()`、`paint()` 的声明丢掉，规则其余部分保留（转义在分词时已解码，`\75rl(` 就是
    `url(`）；`@import`、`@font-face` 等其他 at-rule、带 `:has()` 的选择器、嵌套规则都丢掉；最多 64 KB、
    2000 条规则。输出里没有 `<`（写成 `\3c `），所以样式表不可能结束自己的元素或改变文档结构，变异
    XSS 不依赖 ammonia 对边角情况的处理。重写是幂等的，读信区对清洗后的 HTML 再清洗不会改变它；规则
    只作用于之后下载的正文，已入库的正文保持旧规则清洗的结果。
  - `color_usage` 用同一套分词（`mime/tokens.rs`，展开布局表格也用它）判断正文怎样用颜色（不设、为白底设了颜色、自带深色样式），读信区据此
    决定能否显示成深色（见 [GUI](gui.md#html-引擎)）；
  - 保留表格布局属性和行内 `style`，但可能加载资源的 style（`url(`、`image-set(`、`@import`、
    CSS 转义）整个去掉；
  - 链接只允许 http、https、mailto、cid，`data:` 只允许出现在 `img src`；判断时像 URL 解析器
    一样忽略其中的制表符、换行和开头的控制字符。
  - 图片地址（远程、`cid:`）原样保留，是否加载由渲染端决定，默认应拦截远程图片。
- **内嵌**（`inline`）指 HTML 用 `cid:` 引用了它：按 RFC 2392 取出完整的引用值、百分号解码后精确
  比较（`cid:logo-long` 不会让 `logo` 也算内嵌）。没被引用的图片都算附件，`has_attachments` 只统计
  非内嵌的部分。

## 正确性

- 用 UID 之前先 EXAMINE 并比对 UIDVALIDITY；不一致或服务器已没有这封邮件时返回
  `Error::OutOfSync`，交给下一次同步处理。
- 写入时只检查邮件行是否还在：message id 不复用，UIDVALIDITY 变化会删除旧代际的行，所以行还在
  就说明它仍是下载开始时的那个（文件夹, UID）。
- 正文、附件行、摘要、`has_attachments` 和全文索引在同一个事务里写入（`Messages::store_body`）；
  并发下载同一封邮件时只有一个写入，另一个直接读结果。
- imap-next 会把整条响应读进内存，所以附件是取回后再写入 BlobStore，不是流式落盘。
