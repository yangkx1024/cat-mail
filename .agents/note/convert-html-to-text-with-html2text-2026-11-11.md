# 用 html2text 把 HTML 转成文本，摘要先放 preheader

- 日期：2026-11-11
- 类型：功能

## 背景

#294。HTML 转文本用 mail-parser 的 `html_to_text`，是一遍字节扫描：不看 CSS，营销邮件开头 `display: none`
的预览文字和后面几百个 `&zwnj;&#847;` 填充都会转出来，摘要的 200 个字符全是不可见字符，列表里看着是空的
（本机 106 封）；Mailchimp 没替换的 `*|MC_PREVIEW_TEXT|*` 进了摘要（8 封）；`<div>`、表格、列表挤成一行，
链接地址丢了，MCP 读只有 HTML 的验证邮件拿不到链接。

## 决策

- **`catmail-html` 加 `text` / `text_and_preheader`，用 html2text 0.17.4（`css`）。** 和 `catmail-html`
  用同一个 html5ever 0.40.1，MIT，MSRV 1.85；`css` 只多 `nom 8`（已在树里）。`raw_mode`：表格一格一行、
  不画框线。宽度 `1 << 20`：`usize::MAX` 会让 html2text 内部加法溢出（debug 下 panic）。
- **写法向常见客户端生成的 `text/plain` 看齐**（对照了 Thunderbird / Gecko `nsPlainTextSerializer` 的
  排版模式）：列表 `* ` / `1. `，引用 `> `，链接 `文字 <地址>`；标题、强调不加标记，图片不输出 `alt`
  （html2text 默认的 `[link]`、`[Logo]`、`# 标题` 去掉，要自己实现 `TextDecorator`）。链接文字就是地址
  （去掉 scheme、结尾 `/`、`mailto:` 的 `?subject=…` 后比较）、`cid:` 链接、只有图片的链接不写地址。
  装饰器在链接结束时不知道链接文字，所以链接作为 annotation（`Link`，按 `Rc` 指针比较，同一地址的两个
  链接不合并）挂在文字上，用 `render_to_lines` 取带标注的行，链接的文字结束处再写地址。最初用 Unicode
  非字符插进文字当标记、排完版再扫一遍，/simplify 时换掉（输入不用再清理那几个字符）。
- **摘要先放 preheader，而不是像 issue 原先说的去掉它。** 查了常规做法：preheader 是给收件箱列表看的，
  Gmail、Apple Mail 列表显示它，读信区不显示；后面的填充是为了把正文挤出列表。Dovecot 的 PREVIEW
  （#123）不看 CSS，开头也是它；本地摘要若去掉 preheader，以后先显示服务器摘要、取到正文后会突然换成
  另一段话。所以：正文文本按读信区（去掉隐藏的），摘要 = 隐藏在开头的文字 + 正文。隐藏的开头 = 不看
  CSS 转出的文字里，看 CSS 转出的第一行之前的部分（同一个 DOM 排两遍）；只在开头找，中间藏的（手机版
  菜单等）不算；开头找不到对应行就不算。发件人自己的 `text/plain` 不加。
- **不可见字符在摘要里去掉**（`catmail_html::visible`），不在正文文本里去：正文里的这些字符本来就随隐藏
  元素去掉了。零宽连接符只在两个可见字符之间保留、变体选择符只跟在可见字符后面保留（emoji 由它们组成）；
  本机有 3 封用 `&zwj;` 做填充，最初整体保留 U+200D 时漏掉了它们。
- 摘要还去掉链接地址（`<https://…>`、`<mailto:…>`，HTML 转出的和发件人纯文本里的都是这样写）；
  preheader 和正文开头相同时只说一次。没替换的 `*|…|*` 只从 preheader 里去掉：正文里可见的那种读信区也
  显示，摘要照样显示（本机 Lazada 2 封）。只剩跟踪链接的文本（Indiegogo 11 封）因此没有摘要，
  原来的摘要就是那个链接，没有信息量。
- 放弃：只用 CSS 之后的文字做摘要（与 #123 不一致，也不是常规做法）；自己写选择器匹配找隐藏元素
  （html2text 已经做了，排两遍更简单）。
- `CONTENT_VERSION` 4 → 5：所有 HTML 邮件的文本格式都变了。`sanitize_html` 文档里"proto 依赖只有一个不带
  属性的 `<style>`"的约定不再需要，删掉。

## 方案

- `crates/catmail-html/src/text.rs`：`text`、`text_and_preheader`、`visible`、`split_address`（摘要去地址
  也用它），装饰器 `Plain`，`write_lines`、`same_address`、`preheader`。CSS 读不了时退回不看 CSS 的排版；
  样式里没有 `display`、`overflow` 时不排第二遍（藏不了文字）。
- `crates/catmail-proto/src/mime/content.rs`：`text` 返回文本和 preheader（只有 HTML 时，或文本部分是标记
  改用 HTML 时）；`snippet(text, preheader)`、`snippet_words`、`without_addresses`；指纹语料加 `PREHEADER`。
- 文档：`docs/architecture/bodies.md` 的"正文的组成"（文本转换、新的 `snippet` 一条），crate 说明。
- 测试：`catmail-html` 的 `leaves_out_what_style_sheets_and_attributes_hide`、
  `lays_out_blocks_lists_tables_and_quotes`、`writes_a_link_address_after_its_text`、
  `visible_keeps_what_joins_emoji`、
  `preheader_is_what_is_hidden_in_front`、`no_preheader_without_hidden_text_in_front`；proto 的
  `snippet_starts_with_the_html_preheader`、`snippet_leaves_out_what_takes_no_room_or_is_not_read`。
  macOS 上 `cargo fmt`、`clippy -D warnings`、全部测试、`cargo deny` 通过（系统 cargo，本机没有 Nix）。
- 手动：新旧两版各解析一遍本机 3886 封有原文的邮件（数据库拷贝，临时 example，已删）。release 总时间
  2.5 → 3.3 秒，最慢一封 6.7 ms。/simplify 时有人建议只把影响文字的样式规则交给 html2text（合成的大
  样式表邮件上开 CSS 排版 5.7 ms，只留一条规则 1.0 ms）。在本机 3709 封 HTML 上量了：开 CSS 比不开总共
  多约 0.6 秒，其中行内 `style` 属性约 0.48 秒（每个约 1 µs），匹配样式表只约 0.11 秒；开销最大的几封
  根本没有样式表。裁剪样式表最多省 0.1 秒，不做。文本没有丢失；1675 封文本格式变化（断行、列表、链接地址）；679 封摘要
  变化。看着是空的摘要 106 → 0，隐藏的 `*|…|*` 8 → 0；#280 那批"文本是样式表"的没有回来（像 CSS 的只剩那封讲
  Java 代码的，按设计保留）。抽查账单（Apple、SP Group）、券商月结单、营销邮件，排版和链接正常。
- 另外测了深层嵌套：html5ever 的树构建对嵌套 `<div>` 是二次方的（20 万层清洗 43 秒，html2text 再 61 秒），
  清洗本来就有，没在这里处理；html2text 在 2 MiB 栈的线程上两万层不溢出。
