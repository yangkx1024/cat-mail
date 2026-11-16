# 引文的头存成结构化数据：回复的署名、转发的头部

- 日期：2026-11-16
- 类型：重构

## 背景

issue #314。`Quote::intro` 原来是一段纯文本：回复是 "On …, … wrote:"，转发是分隔行加 `From: …` 几行，由 core 拼好
存进 `drafts.quote_intro`。用到它的地方只能当文字：发信的 HTML 逐行 `<br>`，写信窗口靠 `FORWARD_SEPARATOR` 把头部
从文字里切出来（`Quote::forward_headers`）。头部排不了版（名字加粗、对齐），`From` 这些标签写死在 core 里，将来
本地化只能整段重拼。

## 决策

- **`Quote { kind, intro }` 换成 `Quote { head: QuoteHead }`**：`QuoteHead::Reply(Attribution { from, date })`、
  `QuoteHead::Forward(Vec<(ForwardField, String)>)`。类别就是 head 的变体，`QuoteKind` 删掉：各处渲染对 head
  匹配一次，署名、头部和引用方式（回复进 blockquote、转发不缩进）一起决定。
- **存名字的键，不存 "From"**：`ForwardField` 枚举，文字由 `ForwardField::label` 给（现在只有英文）。项目还没有
  i18n，成本和存文字一样，以后按语言换措辞不用改草稿。
- **值存建草稿时写好的文字，不存 `Address` 和时间戳**：快照本来就是用户当时看到的样子；存时间戳的话
  `catmail-types`（`as_text` 在这里）就要引入 `jiff` 和系统时区。代价是日期格式、地址链接不能显示时再变，真要做时
  再把值换成带类型的，结构不用动。
- **回复的署名也拆**（issue 的待定项）：和标签同理，"On …, … wrote:" 的措辞在 `Attribution` 的 `Display` 里，
  没有发件人时是 "someone"，没有日期时省掉 "On …,"。
- **存储**：`quote_kind`、`quote_intro` 两列换成一列 `quote_head`，整个 `QuoteHead` 的 JSON（`json_valid` 检查），
  类别是它的标签。考虑过留 `quote_kind` 列、JSON 只放这一类的内容：读写两头都要按类别手写分派，还要为这一列留着
  `QuoteKind` 的 `as_str` / `FromStr`；没有 SQL 按类别查，单独一列没有用处。不认识的类别读出时 serde 报错。
- **CLI、MCP 不变**：它们输出 `Quote::as_text` 的纯文本（`DraftSummary.quote`），agent 读文字就够，没有用得上结构
  的地方；schema 快照不动。issue 里列了这一项，等有用处再加。
- `FORWARD_SEPARATOR` 只在显示成文字时用（纯文本、HTML、在引文里回复），不再是 core 和 frontend 之间的约定；
  `Quote::forward_headers` 删掉。

## 方案

- `catmail-types`（`draft.rs`）：`QuoteHead`、`Attribution`、`ForwardField`；`Quote::as_text` 按 head 写纯文本；
  `QuoteHead::values_mut`（core 清 NUL 用，不必知道 head 的形状）。
- `catmail-store`：`0001_init.sql` 的 `quote_head` 列，`drafts.rs` 存取它的 JSON。
- `catmail-core`（`drafts.rs`）：`attribution`、`forward_content` 填结构；`without_nul` 清 head 里的每个值。
  `display::local` 只剩 `date` 一个调用者，并成 `display::local_date`（没有时间时 `None`，署名直接用）。
- `catmail-proto`（`mime/build.rs`）：`quote_html` 回复写署名，转发写分隔行和头部表格（名字加粗一列，内联样式，
  收信端没有我们的样式表）。
- `catmail-frontend`：`quote_heading` 用署名；`quote_to_html`（在引文里回复）把转发头部写成一行一段、名字加粗
  （编辑器的 schema 没有表格）。
- `catmail-gui`（`composer/quote.rs`）：转发头部按名字、值两列排，用草稿栏显示收件人的同一个 `mailbox::panes::fact`。
- 文档：`sending.md`（引文的头为什么是数据、怎么存）、`gui.md`。
- 测试：署名的三种情况；`values_mut`；转发纯文本（有头部、没有头部）；`Quote` 的 serde 往返；store 存取回复和转发的 head；
  core 的 `forward_content` 头部内容和顺序；proto 发出的转发 HTML 表格；`quote_to_html` 的转发头部；原有回复、转发的
  纯文本和 HTML 测试按新署名格式更新。写信窗口的头部布局没有驱动窗口的测试。
