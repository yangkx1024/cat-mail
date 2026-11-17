# 写信窗口的图片只在显示处拦远程的，粘贴去掉发不出的图

- 日期：2026-11-17
- 类型：修复

## 背景

#321。远程图片的拦截（原 `Images::to_files`）只在 `rich::parse` 里跑，也就是打开草稿和在引文中回复时。别的路进
文档的图片都绕过了它，nsh 自带的图片显示（`image_of` 直接 `image(src)` / `image_size(src)`）当场去取：

- 粘贴：nsh 的 `paste`（`transform_pasted_html` → `DomParser` → `transform_pasted`），写信窗口没装插件。从网页、
  别的客户端复制带 `<img src="https://…">` 的内容，跟踪像素生效。
- 输入 Markdown 的 `![x](https://…)`：starter kit 的 image 输入规则直接建图片节点。
- 粘贴里的读信区 `catmail-image:…`、别的邮件的 `cid:` 原样存进草稿，收件人看到坏图；本机文件（`file:`、路径）
  编辑器从磁盘读来显示，存草稿时原样写进 HTML，本机路径（含用户名）随信发出。

## 决策

- 先做了按入口打补丁的版本（粘贴插件把 `http(s):` 换成 `catmail-blocked:`），审查时发现输入规则这条路漏了：
  每多一个入口就要多挂一次转换。改为在显示处拦：给 `image` 节点注册 NodeView（`ImageView`），只加载
  `Images::shown` 给的（草稿自己的 `cid:` → 文件、`data:`），别的画成按宽高留出的空框。
- 文档里从此就是要发出的内容：`cid:`、远程 URL 原样。原来那套双向改写（`to_files` / `to_content_ids`、
  `catmail-blocked:` 地址、`blocked` 和 `ids` 两张表）整个删掉，`parse` 和 `written` 也不再绕 JSON 一圈。
  `Images` 只剩 Content-ID → 文件。草稿里本来就不会存 `catmail-blocked:`（存时已换回），不需要迁移。
- 「去掉草稿发不出的图」只对粘贴进来的内容有意义（草稿自己的 HTML 里不认识的 `cid:` 是它自己的部分），所以仍是
  粘贴插件（`transform_pasted`），但只删不改：留下草稿自己的 `cid:`、`data:`、`remote_image_url` 认的
  `http(s):`，其余去掉。issue 建议「`catmail-blocked:` 丢掉」，这种地址现在不存在了。
- 自己画图要照 nsh 私有的 `image_box` 写一份尺寸计算（缺的一边按自然尺寸的宽高比）。图片是 inline 节点，nsh
  对 node view 和自带图片都包同一个 `inline_box(view_box)`，选中的样式在外层，行为不变。
- 判断「远程」用 `catmail_html::remote_image_url`（读信区图片缓存用的同一个），在 `catmail_core::html` 里导出。
  `crate::shell::lock` 改为 `pub(crate)`，编辑器、node view、粘贴插件共用的 `Arc<Mutex<Images>>` 用它取锁
  （插件和 node view 要 `Send + Sync`）。

## 方案

- `catmail-frontend/src/composer/rich_text.rs`：`Images::shown`、`Images::drop_unsendable`（`retain_images`
  删节点）；删 `to_files`、`to_content_ids`、`BLOCKED_SCHEME`。
- `catmail-gui/src/composer/rich.rs`：`ImageView`、`image_box`、`PastedImages`；`insert_image` 直接写
  `cid:`；`parse`、`written` 去掉图片改写。
- 测试：frontend 的 `only_the_drafts_images_and_data_urls_are_shown`、
  `pasted_images_keep_only_what_the_draft_can_send`；gui 的 `shown` 助手先断言 `image` 的 node view 注册了，
  再按 `Images::shown` 给出每张图显示的来源。`remote_images_are_not_loaded_but_sent` 补了 Markdown 输入规则
  （调插件的 `handle_text_input` 输入 `)`），`pasted_images_are_not_loaded_and_only_sendable_ones_are_sent`
  按 nsh 粘贴的顺序（剪贴板解析器再 `transform_pasted`）粘进编辑器。
- 未在 app 里实际看过：图片的选中、删除、光标前后移动，远程图片的空框。读信区复制时 WebKit 往剪贴板放的是不是
  改写后的 `catmail-image:` src 也没确认；两种都被处理。
