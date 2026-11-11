# 文本部分是标记时改用 HTML 转出的文本

- 日期：2026-11-11
- 类型：修复

## 背景

#280：有些邮件的正文文本、列表摘要和全文索引全是 CSS（例如 Keychron 的购买确认信），MCP 的 `read_message`
读到的也是样式表，agent 看不到正文。issue 认为原因只在发件人坏掉的 `text/plain`。查本机数据（只读拷贝）后发现
有两个原因：

1. 本机 43 封"文本像 CSS"的邮件里 32 封根本没有 `text/plain`，文本是我们从 HTML 转的。mail-parser 的
   `html_to_text` 只在标签只有名字时才认 `<style>`，`<style type="text/css">` 不被跳过，里面的规则成了文本。
   它跳过 `<head>` 的方式也和浏览器不一样，有的邮件只转出了开头几十个字。
2. 发件人的 `text/plain` 坏了：有的是样式表（Keychron、Squarespace），有的是整份 HTML 源码（PayPal 月结单、
   影院购票确认、Lemon Squeezy 等），全库约 25 封，比 issue 估计的多。

## 决策

- HTML 转文本改用清洗后的 HTML：清洗后只剩开头一个不带属性的 `<style>`，mail-parser 能正确跳过，转出的就是
  读信区显示的文字。只有 HTML 的邮件直接转已有的 `html`，不重复清洗。
- 清洗时把 `<title>` 连同文字去掉：ammonia 去掉标签但保留文字，标题会成为正文第一行（读信区原本也显示它）。
- `text/plain` 是否是标记：看开头 1000 个非空白字符，落在标签、`/* */` 注释或花括号块里、且 HTML 转出的文字里
  没有的词占一半以上，就改用 HTML 转出的文本。
  - 只按标记比例判断会误伤：`<https://…>` 链接和代码（GitHub 评论、Java 求助邮件）在全库扫描里都达到 0.8。
    要求"HTML 里看不到"后，讲代码的邮件都是 0。
  - 只看全文比例会漏掉 Keychron（样式表在前、正文在后，全文只有 0.31），所以看开头。
  - 全库 2042 封同时有文本和 HTML 的邮件：阈值以上的都是坏的；0.38–0.47 是 GitHub 机器人评论（markdown 里夹着
    `<!-- -->`、`<details>`）和 Google 通知，留着原文本。误判代价小：HTML 是同一封信，换过去最多丢排版。
- 放弃：有 HTML 就一律从 HTML 转（纯文本通常更好）；给 mail-parser 打补丁（清洗后的 HTML 已经够用）。
- `CONTENT_VERSION` 升到 3。没有原文的正文重建时文本不动（不知道它来自哪里）；本机 43 封都有原文。

## 方案

- `catmail-proto/src/mime/content.rs`：`text`、`is_markup`、`markup_spans`（按字节扫描，线性，不在非字符边界
  切片，没有结尾的注释记住后不再找）、`is_tag_start`；`CONTENT_VERSION = 3`，指纹语料加入 `STYLE_AS_TEXT`。
- `catmail-html/src/sanitize.rs`：`add_clean_content_tags(["title"])`。
- 文档：`docs/architecture/bodies.md` 的"正文的组成"、清洗规则和"按版本重建"。
- 测试：`a_text_part_of_markup_gives_way_to_the_html`、`html_text_leaves_out_style_sheets_and_the_title`（不改代码时
  都失败），`a_text_part_about_markup_is_kept`（去掉"HTML 里看不到"的条件时失败），`markup_spans_are_tags_comments_and_blocks`；
  `strips_active_content_and_keeps_layout` 加上标题。
- 手动：用新代码重新解析本机 3886 封有原文的邮件（release 3.7 秒），235 封文本变化：62 封从标记变成正文，147 封
  只是开头少了标题或样式，其余是 mail-parser 截断的正文变完整、`style`/`target` 等属性碎片消失。少数开头多出
  浏览器也会显示的内容（`<!-->` 之后的 Word 元数据，Mailchimp 隐藏的预览文字）。
- 未处理：有的营销邮件开头是大段不可见的填充字符（`&zwnj;` 等），摘要仍然看起来是空的；改用 html2text 按 DOM
  和 CSS 转文本见 #294。mail-parser 1.0 已修好带属性的 `<style>`，升级见 #293。
- review 后：`is_markup` 先数开头落在标记里的字符，不到一半就不转 HTML；`text` 用 mail-parser 的
  `text_bodies()` / `is_text_html`；`sanitize_html` 的文档写明"只有开头一个不带属性的 `<style>`"是 proto 依赖的约定。
