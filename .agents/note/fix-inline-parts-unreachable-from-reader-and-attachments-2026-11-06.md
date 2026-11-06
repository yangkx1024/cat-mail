# 修正内嵌部分的判定：不在正文里显示的都列为附件

- 日期：2026-11-06
- 类型：修复

## 背景

#249。一个部分算不算内嵌（`Attachment::inline`），解析时看正文 HTML 里有没有出现 `cid:<它的 Content-ID>`（按子串找）；
附件列表排除内嵌的，读信区却只加载内嵌、`image/*` 的部分。两套规则对不上，有的部分两边都不收，用户在界面上拿不到：

- `<a href="cid:report@x">` 指向的 PDF：算内嵌，不进附件列表；正文不加载它，链接点了也没反应。
- 正文文字里恰好写着 `cid:note@x`：按子串匹配，对应的部分也算内嵌。
- 标成 `application/octet-stream` 的图片被 `<img>` 引用：算内嵌，读信区却因为不是 `image/*` 不加载，显示坏图。
- `<img>` 引用的视频：算内嵌，正文里是坏图，附件列表也没有。

## 决策

- 规则只有一处：`catmail_proto::mime::is_inline`。内嵌 = `<img src="cid:…">` 引用它（`catmail_html::cid_image_ids`，
  用清洗器自己的图片钩子取，和页面脚本报的 `img[src]` 一致），并且能当图片显示（`image/*` 或
  `application/octet-stream`）。其余的都列为附件。读信区、附件列表、CLI、转发都只看 `inline`
  （新加的 `MessageBody::inline_parts` / `files`），读信区去掉自己的 `image/*` 判断。
- `cid:` 链接指向的部分选择列为附件，而不是让链接能打开它：附件列表已有保存功能，不用给页面的链接加新动作。
- 标成 `application/octet-stream` 的内嵌部分在做 `data:` URL 时按开头字节认类型。proto 里取远程图片已经有一份同样的
  识别（`http::sniff`），移到 `catmail_html::sniff_image` 两处共用；内嵌图片也和远程图片一样以字节为准，标错类型的也能对上。
  octet-stream 其实不是图片的，正文里仍是坏图、也不列出；只凭类型名判断不了，没再加读内容的判定。
- 已存的正文要按新规则重判，`CONTENT_VERSION` 升到 2。有原文的重新解析；没有原文的（`refresh_html`）原来按
  Content-ID 集合标内嵌，改为传入判定函数，core 用同一个 `is_inline` 判（要看部分的类型）。指纹没变：样例邮件里
  没有非图片的 `cid:` 引用。
- 取图片引用要再用清洗器解析一遍 HTML（原来是按子串找）。解析时只在有部分可能内嵌（有 Content-ID、类型可能是图片）
  时才做，多数邮件没有这样的部分；图片地址只在钩子里看，不再逐个复制（`data:` 的可能很大）。考虑过在清洗的同一遍里
  收集：ammonia 调钩子在去掉不合规节点之前，被去掉的 `<img>` 也会让部分算内嵌，重建时再清洗又对不上，没有采用。

## 方案

- `catmail-html/src/images.rs`：`cid_image_ids`、`sniff_image`；`image_sources` 改为在钩子里转换（远程图片、
  Content-ID 各取所需）；`InlineImage::new` 先按字节认类型。
- `catmail-proto`：`mime::is_inline`，解析时先建好部分再判内嵌；`sanitized_html` 只给无原文的重建用；去掉按子串找的
  `cid_references`；
  `http.rs` 用 `sniff_image`。
- `catmail-store`：`Messages::refresh_html` / `Attachments::mark_inline` 接受判定函数；`catmail-core/src/rebuild.rs` 传 `is_inline`。
- `catmail-types`：`MessageBody::inline_parts`、`files`；gui 读信区、frontend `reader::attachments`、CLI `read` 和
  输出、`drafts` 转发改用它们。
- 文档：`docs/architecture/bodies.md` 的内嵌规则、`gui.md` 的内嵌图片。
- 测试：proto `inline_means_shown_as_an_image` 覆盖链接、文字、octet-stream 图片和视频四种（修之前四个都算内嵌，
  在 HEAD 上跑确认失败）；html `only_images_make_parts_inline`、`an_inline_image_is_of_the_type_its_bytes_are`；
  `sniffs_raster_formats` 随函数移到 html；core 的无原文重建测试把部分改成 `image/png`。macOS 上 fmt、clippy、
  全部测试（809 个）通过。
