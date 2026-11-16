# 写信窗口显示引文：收起的横栏、和读信区一样的预览、去掉引文、在引文里回复

- 日期：2026-11-15
- 类型：功能

## 背景

issue #303 的第二步。第一步（PR #313）让引擎把回复、转发的原信作为 `DraftContent::quote` 和正文分开存、发信时
拼接，但写信窗口还看不到引文，也去不掉它；frontend 的 `text_to_html` 还把 `>` 开头的行猜成引用。

## 决策

- **引文在编辑器外面**：编辑器下面一条横栏（展开 / 收起、开场白、在引文里回复、去掉引文），默认收起。展开后和
  正文各占一半高度，原信在 web view 里，和读信区完全一样（页面、CSP、深色、远程图片的规则）。考虑过编辑器和引文
  同在一个滚动区（像 Gmail 移动端）：nsh 的编辑器只把光标滚进自己的滚动区，外面再套一层时打字打到底下会看不见
  光标，放弃。
- **读信区的 web view 抽成 `web.rs`**，两处共用：web view 的设置、图片应答和限流的看守、远程图片的下载（缓存的
  先显示、出错的提示、按钮）、外观。"哪些远程图片还挡着、是否在下载"是 `catmail_frontend::html::RemoteImages`
  （有测试），`BodyPages::page` 只返回还没显示的远程图片。引文的 web view 第一次展开时才建，收起只是藏起来。
  每个视图各在自己的线程，各有一份（nsh 的 global）。读信区剩下的是它自己的：换邮件的隐藏、滚回顶部、
  在浏览器打开。
- **去掉引文随保存走**：`Written::quoted`，`Written::changes` 在去掉时给 `quote: Some(None)`，否则和原来一样
  不动引文（不每次交回整封原信）。"在引文里回复"把引文接进正文和去掉引文在同一次保存里，草稿不会两边都没有或
  有两份。去掉引文不能撤销（和删附件一样）；接进正文的那一步不进编辑器的撤销历史，否则撤销后引文就丢了。
- **`>` 的识别只剩一处**：`catmail_html::quoted_text_html`（按层数嵌套引用），proto 发信和"在引文里回复"纯文本
  原信时用。`text_to_html` 只做一行一段，用户自己写的 `>` 是文字。
- 转发的横栏写 "Forwarded message"，头部几行（From、Date…）在展开后的原信上面；回复的横栏是开场白。转发开场白
  的分隔行是 `catmail_types::FORWARD_SEPARATOR`，core 用它写、`Quote::forward_headers` 用它取，frontend 不解析
  引擎写的字符串。
- 预览只读引文 HTML 显示的图片（`cid_image_ids`），不读用户自己贴进正文的。
- 没合并的：回复引文的两种 HTML（proto 发信用的、"在引文里回复"进编辑器的）结构相同、细节不同（`<br>` 与空段落、
  样式表），暂时各写一份；进编辑器的用 `nested` 明确去掉原信的样式表，不靠编辑器的解析器忽略 `<style>`。
- "一张也没取到才算失败"在 `RemoteImages::came` / `ended` 里（有测试），`web::fetch_remote_images` 只转发结果。
- 预览页在视图线程上生成（`serve_images`），没挪到后台：release 下 90 KiB 的 newsletter 约 2.5 ms，440 KiB 约
  13 ms，1.8 MiB 约 58 ms（深色多约 25%），只在第一次展开和换外观时各一次。
- 开窗时不再复制引文：视图借用 `Opening`，`Written::of` 逐个字段构造。
- 转发头部的结构化另开 #314。

## 方案

- `catmail-types`：`FORWARD_SEPARATOR`、`Quote::forward_headers`。
- `catmail-html`：`quoted_text_html`（从 proto 移过来；`cite` 是 blockquote 的属性，`empty` 区分发信的 `<p><br></p>`
  和编辑器的 `<p></p>`）。
- `catmail-frontend`：`html::RemoteImages`；`Written::quoted` / `changes`、`Fields::quoted`、`quote_heading`；`rich_text::text_to_html` 去掉
  `>` 识别，`quote_to_html`，`Images::path`。
- `catmail-gui`：`web.rs`（从 `reader/` 移出）；`composer/quote.rs`（状态、横栏、预览、去掉、在引文里回复）；
  `Rich::answer_in`（不进历史的插入），`parse` 与打开共用。
- 文档：`gui.md`（目录、正文在 web view 里、写信窗口的引文）、`sending.md`（改动、`>` 的识别）、ROADMAP。
- 测试：`quoted_text_html` 的嵌套；`text_to_html` 不认 `>`；`quote_to_html`（回复带嵌套引用、转发不缩进）；
  `answer_in` 接在末尾、图片从文件显示、撤销不掉、样式表不进正文；`Written::changes` 只在去掉时动引文；
  `quote_heading`；`forward_headers`；`RemoteImages` 的按钮何时出现、何时再出现。界面没有驱动窗口的测试。
