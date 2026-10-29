# 把 HTML 清洗移到独立的 catmail-html crate

- 日期：2026-10-29
- 类型：重构

## 背景

HTML 清洗、样式表重写（`css.rs`）、`color_usage`、图片相关的处理和刚加的深色配色（`dark.rs`、
`recolor_html`）都放在 `catmail-proto` 的 `mime/` 下。proto 自己只用到 `sanitize_html`（解析正文时
清洗）和 `content_id_from_url`，其余都只给 GUI 用，经 core 转出去；`DarkPalette` 这种纯展示类型也就
出现在协议层的公开 API 里，proto 的职责（IMAP / SMTP / MIME 适配）越来越模糊。

## 决策

- 单独建 `catmail-html`：纯函数、同步、无 IO，依赖只有 ammonia、cssparser、html5ever、base64、url。
  proto 依赖它在解析时清洗，core 依赖它重新导出给前端（前端仍只经 core，不改 AGENTS.md 的规则）。
- 不把深色配色单独挪到 `catmail-render`：它要用清洗器的 CSS 记号重写，render 不能依赖 proto，挪过去
  要么重复一份安全相关的代码，要么在 Blitz 解析后改 DOM（Blitz 解析时就处理了 `<style>`，不可行）。
  放在一起，读信区的几遍处理和清洗用同一个清洗器，不会放进清洗不允许的东西。
- `html.rs` 顺便拆成 `sanitize.rs`（清洗、颜色）和 `images.rs`（图片来源、内嵌、`cid:` 地址），
  `content_id_from_url` 从 proto 的 `content.rs` 移过来。

## 方案

- 只移动和调整可见性，不改行为。`crates/catmail-html/src/{lib,sanitize,images,css,dark,tokens}.rs`；
  proto 去掉 ammonia / cssparser / html5ever 依赖；core 的 `html` 模块、`images.rs`、
  `image_cache.rs` 改为从 `catmail_html` 导入。
- 文档：AGENTS.md、`docs/architecture.md` 的分层表和依赖方向、README 的 crate 表、`bodies.md`。
- 验证：测试随代码移动，全部测试、clippy、fmt、`cargo deny check` 通过；`cargo doc` 无断链。
