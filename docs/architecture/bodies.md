# 正文与附件

[← 架构](../architecture.md)

正文按需下载（`MailClient::download_message`），不在同步时预取。所有按需操作经过同一个入口
（`catmail-core/src/body.rs` 的 `on_server`）：从连接池取连接、EXAMINE 比对 UIDVALIDITY（连接已经
只读打开着同一个文件夹、UIDVALIDITY 一致时省掉，见[连接池](protocol.md#连接池)）、在账户的取消信号上
select。

## 两条下载路径

按 RFC822.SIZE 分（界限默认 2 MiB，即 `EngineTuning::whole_message_limit`，测试和按流量计费的
用户可以在 builder 上调）：

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

## 原始字节的邮件头

有的邮件程序把附件文件名、主题、显示名直接用发件人本地的字符集（如 GBK）写进邮件头，不按 RFC 2047 / 2231
编码。mail-parser 和 imap-proto（BODYSTRUCTURE）都按 UTF-8 有损读，不是 UTF-8 的字节变成 `U+FFFD`。

- 转码在 `mime::transcode_fields`：一个字段一个字段地看，合法 UTF-8 的字段原样保留（其中的 `U+FFFD` 是发件人
  自己写的），其余的用线索里第一个能完整解码（encoding_rs 不做替换地解码成功）的字符集转成 UTF-8，再整体重新
  解析。先转码再解析：GBK 等双字节的第二个字节可能是 `\` 或 `"`。
- 线索是同一封邮件里文本的 `charset`（其他客户端也这样猜），按 MIME 顺序取所有文本部分（`text/*`）的；整封邮件、替身邮件和 BODYSTRUCTURE（同步时的摘要邮件头，见
  [同步](sync.md#邮件头)）用同一条规则。附件文件名先试这个部分自己的 `charset`。没有线索就保持原样，不做
  字符集探测。
- 只用 encoding_rs 认识、ASCII 兼容的编码（`legacy_encodings`）：UTF-16、ISO-2022-JP 会把字段名、冒号和换行
  也当成文字解码，解得通却毁了邮件头的结构。UTF-8 不算（这些字段不是）；ASCII 也不算，它说明不了发件人用
  什么字符集，而且按 WHATWG 它的标签（`us-ascii`、`ascii`、`ansi_x3.4-1968`）指 windows-1252，什么字节都
  解得通。其他 ASCII 别名 encoding_rs 不认识，自然不用。
- 附件文件名只在含 `U+FFFD` 时重读。整封下载时原始头就在原文里；按部分下载时，BODYSTRUCTURE 里参数值带
  `U+FFFD` 的部分（`MimeNode::garbled_sections`）另发一条 `BODY.PEEK[<section>.MIME]`，取回的头交给同一套
  逻辑。这条 FETCH 解析不了就只是文件名保持原样；只有这种邮件多一次往返，不并进取正文的那条（那条缺项会
  退回整封下载）。

## 正文的组成

- `text`：所有正文文本部分；只有 HTML 时由 HTML 转换。
- `html`：只在有真正的 HTML 部分时存在，经 `ammonia` 清洗：
  - 去掉脚本、表单、iframe、事件属性和 `javascript:` 链接；
  - 样式表不交给 ammonia（它不看 `<style>` 里面，写回时原样输出原始文本）：分词时按树构建器的规则
    （`script`、`textarea` 等之后切到原始文本，SVG / MathML 里的忽略）取出 `<style>` 的内容，经
    `mime/css.rs` 用 cssparser 的记号逐条重写，合成一个 `<style>` 放在输出开头。每张表单独解析（没写完
    的规则随它的表结束，不吞掉下一张）；带 `media` 属性的表包进 `@media <属性值>{…}`，属性值按
    `@media` 的条件重写，不是媒体查询列表就整张不用（浏览器也当它 `not all`）；`type` 不是空或
    `text/css` 的（模板等）浏览器不应用，这里也跳过。记号写回时按 cssparser 的
    `needs_separator_when_before` 在必要处插入 `/**/`：注释在分词时就没了，`u/**/rl(` 直接拼起来会
    变成没检查过的 `url(`，`1px/**/2px` 会变成一个尺寸。只保留普通规则和
    `@media` / `@supports`（最多嵌套三层）；值里有 `url()`、`image()`、`image-set()`、`cross-fade()`、
    `src()`、`element()`、`paint()` 的声明丢掉，规则其余部分保留（转义在分词时已解码，`\75rl(` 就是
    `url(`）；`@import`、`@font-face` 等其他 at-rule、带 `:has()` 的选择器、嵌套规则都丢掉；最多 64 KB、
    2000 条规则。输出里没有 `<`（写成 `\3c `），所以样式表不可能结束自己的元素或改变文档结构，变异
    XSS 不依赖 ammonia 对边角情况的处理。重写是幂等的，读信区对清洗后的 HTML 再清洗不会改变它；规则
    只作用于之后下载的正文，已入库的正文保持旧规则清洗的结果。
  - `color_usage` 用同一套分词（`mime/tokens.rs`）判断正文怎样用颜色（不设、为白底设了颜色、自带深色样式），读信区据此
    决定能否显示成深色（见 [GUI](gui.md#html-引擎)）；
  - 保留表格布局属性和行内 `style`，但可能加载资源的 style（`url(`、`image-set(`、`@import`、
    CSS 转义）整个去掉；
  - 链接只允许 http、https、mailto、cid，`data:` 只允许出现在 `img src`；判断时像 URL 解析器
    一样忽略其中的制表符、换行和开头的控制字符。
  - 图片地址（远程、`cid:`）原样保留，是否加载由渲染端决定，默认应拦截远程图片。
- **哪些是远程图片**由 proto 决定（`mime::remote_image_urls`，经 `catmail_core::html` 导出）：清洗用的
  同一个 ammonia 配置列出 `img src`（`image_sources`），只留 `http` / `https`，按 `url` 解析后的写法
  （`remote_image_url`）去重，最多 200 张。下载、缓存和读信区都用这一种写法，调用方传别的写法，缓存
  也会先规范化。
- **远程图片**只在前端要求时下载（GUI 是用户点"Show remote images"），`MailClient::fetch_remote_images`
  在引擎运行时上用 `catmail-proto` 的 `http::ImageFetcher`（reqwest，TLS 与邮件服务器共用同一个
  rustls + ring 配置）：
  - 只取 `http` / `https`，重定向最多 5 次且只能到这两种 scheme；只发 GET，不存 cookie，不发 `Referer`
    （重定向时也不发），User-Agent 是 `cat-mail/<版本>`。
  - 每张 30 秒、10 MiB 为限（先看 `Content-Length`，再边读边数）；一次最多 200 张，同时 6 个。
  - 类型先按开头字节认常见栅格格式（服务器常标成 `application/octet-stream` 或标错），认不出再用
    `Content-Type` 的 `image/*`，都不是就算失败。SVG 也算失败：读信区不显示 SVG（解码器会读本地
    文件），不必缓存。
  - 返回一个流（`RemoteImageStream`）：每个 URL 下载完（或失败）就给出它的结果，按完成顺序，一张
    失败不影响其他；`next_ready` 不等待地取已完成的，前端可以把同时到的几张合成一批。流是 tokio 的
    channel，任何执行器都能等。丢掉流，引擎里还在下载的立即中止（`JoinSet` 随任务丢弃）。
  - 下载成功的图片进缓存（`image_cache.rs`，下面）；缓存里有的直接从缓存给出，不发请求。
  - 不拦截局域网和本机地址：与浏览器打开网页相同，且只在用户点击后、只对这封信的图片 GET。
- **远程图片缓存**在数据目录的 `remote-images/`，一个 URL 一个文件（文件名是 URL 的 SHA-256，内容是
  MIME 类型、换行、图片字节），第一次存入时建目录（0700，文件 0600）。
  - 不放进 `BlobStore`：blob 按内容寻址，可能与同内容的附件共用一个文件，只能在 GC 宽限期过后删；
    清除缓存要求立刻删掉。缓存文件没有别的引用，任何时候删掉都安全（缺了就是没命中）；写入经临时
    文件（`.tmp` 开头，读时忽略）改名，读到格式不对的文件就删掉当作没命中。
  - 缓存的图片可以不经用户同意直接显示（`MailClient::cached_remote_images`）：不发请求，发件人得不到
    任何信号。缓存按 URL，不按邮件：另一封信引用同一个 URL 也直接显示。不看 `Cache-Control`：
    邮件里的图片基本不变，按 `no-store` 再去取反而又给发件人一次信号。
  - 总大小上限 256 MiB，有新图片存入的那次下载结束后按最后使用时间（读取时更新 mtime）从最旧的删起。`image_cache_size`、`clear_image_cache` 给设置界面用。
- **内嵌**（`inline`）指 HTML 用 `cid:` 引用了它：按 RFC 2392 取出完整的引用值、百分号解码后精确
  比较（`cid:logo-long` 不会让 `logo` 也算内嵌）。没被引用的图片都算附件，`has_attachments` 只统计
  非内嵌的部分。

## 正确性

- 用 UID 之前先 EXAMINE 并比对 UIDVALIDITY；不一致或服务器已没有这封邮件时返回
  `Error::OutOfSync`，交给下一次同步处理。没有这封邮件时先用 `UID SEARCH UID <uid>` 确认（答复里只是
  缺了某个部分不算），确认了就像同步那样删掉本地这一行（`Messages::delete_gone`，行仍在服务器上那个文件夹、
  UIDVALIDITY 和 UID 才删：本地移动过来、还没移动的行不删；查位置之后移动才做成的也不删，它已经在
  目标文件夹里，UID 和 UIDVALIDITY 可能碰巧相同）并发 `MessagesChanged`：否则它要留在列表和
  搜索结果里，直到下次同步那个文件夹，每次打开都失败。
- 写入时只检查邮件行是否还在：message id 不复用，UIDVALIDITY 变化会删除旧代际的行，所以行还在
  就说明它仍是下载开始时的那个（文件夹, UID）。
- 正文、附件行、摘要、`has_attachments` 和全文索引在同一个事务里写入（`Messages::store_body`）；
  并发下载同一封邮件时只有一个写入，另一个直接读结果。
- 连接层把整条响应读进内存（见 [IMAP / 连接层](protocol.md#连接层)），所以附件是取回后再写入 BlobStore，不是流式落盘。
