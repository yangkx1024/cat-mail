# 深色建页把改色并进 serve_images，只清洗一遍

- 日期：2026-11-09
- 类型：重构

## 背景

Issue #261：深色主题下读信区每次建页（打开邮件、切换外观）都要在 GUI 线程上清洗正文两遍：先
`recolor_html` 改色，再 `serve_images` 改写图片地址，后者还对已经改过色的样式表再跑一遍
`css::sanitize`。浅色只有一遍。大的 newsletter 每遍要几毫秒。

## 决策

- `serve_images` 接受可选的调色板，返回值 `Served` 多一个 `dark`（页面该不该用深色）。改色的回退
  规则不变：样式表自带深色样式的不改色、页面仍是深色；样式表里有 `var()` 的不改色、页面浅色；
  属性里的 `var()` 只能在遍历时发现，这时再做一遍只改图片的（和原来 `recolor_html` 白做一遍的代价相同）。
- 图片钩子由工厂函数每遍新建（ammonia 的 `Builder` 拿走钩子的所有权），约定每个钩子从头开始；`serve_images` 新建时清空远程图片列表。
  两遍看到的图片相同、列表本来就去重，所以不清空结果也一样，清空只是不依赖这个前提。
- 页面颜色依赖清洗结果，原来 GUI 先算 `page_dark` 再建页的顺序不再成立。改成 frontend 的
  `BodyPages::page` / `blank` 接受 `Appearance`（浅色的颜色，深色主题下再加 `DarkAppearance`：调色板和深色的颜色），
  `body_page` 按 `Served::dark` 挑颜色。原来 GUI 里的 `page_is_dark` 规则因此搬进 `catmail-frontend`，
  不需要 GTK 也能测。
- `recolor_html` 不再公开（`catmail-core` 也不再 re-export），换成 crate 内部的 `clean_for(html, on_image, dark)`：浅色、深色都走它，回退规则集中在一处。

## 方案

- `crates/catmail-html/src/sanitize.rs`：`clean_for(html, on_image, dark)`，返回 HTML 和是否深色。
- `crates/catmail-html/src/images.rs`：`serve_images(html, base, dark)`，`Served::dark`。
- `crates/catmail-frontend/src/html/page.rs`、`pages.rs`：`Appearance` / `DarkAppearance`，`body_page` 按 `dark` 选颜色。
- `crates/catmail-gui/src/reader/mod.rs`：去掉 `recolor_html` 和 `page_is_dark`，`page_dark` 取自 `Served::dark`。
- 文档：`docs/architecture/bodies.md`、`gui.md` 的深色一节。

验证：原来的改色测试改用新接口，结果不变；新增 `serves_images_and_moves_colors_in_one_pass`
（改色、自带深色、样式表 / 属性里的 `var()` 退回浅色，图片照样改写，远程列表与浅色一致）和
`a_dark_page_unless_the_messages_colors_cant_be_moved`（取代 GUI 的 `page_is_dark` 测试）。
合成的 314 KB 表格布局 newsletter（release，30 次中位数）：深色建页 13.5 ms → 7.6 ms，浅色 5.8 ms 不变。
