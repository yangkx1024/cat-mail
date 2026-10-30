# 深色重着色时 style 属性只解析一遍

- 日期：2026-10-30
- 类型：重构

## 背景

issue #168：`recolor_html` 先用 `styles(html, true)` 再分词一遍、收集所有 `style` 属性，逐个跑
`css::sanitize_style` 只为看有没有 `var()`，结果丢掉；随后清洗时 `attribute_filter` 又用同一个调色板
重写同样的属性。深色主题下每次显示多一遍属性解析和取色（`style` 多的营销邮件约数百微秒到 1 ms），
两处调用的参数也可能渐渐不一致。

## 决策

让清洗时那一次 `sanitize_style` 自己报告用了 `var()`：`sanitizer` 的深色参数从 `DarkPalette`
换成 `Recolor`（调色板加一个 `Arc<AtomicBool>`，ammonia 要求过滤器 `Send + Sync`），清洗完看标志，
置位就返回 `None`。代价是用了 `var()` 的少见邮件会先清洗一遍再放弃；样式表里的 `var()` 仍在清洗前
判断，不受影响。

## 方案

- `crates/catmail-html/src/sanitize.rs`：新增 `Recolor`；过滤器在 `colors_from_variables` 时置位；
  `recolor_html` 清洗后按标志返回；`styles()` 去掉属性收集模式（`Sink.attributes`、`Styles`），
  改名 `style_sheets()` 只返回样式表。
- `with_sheet` 用 `Document::write_to` 直接序列化到已写入样式表的缓冲区，不再经 `to_string()`
  （ammonia 的 `Display` 先序列化到自己的 `Vec` 再转字符串）后复制一遍；`sanitize_html` 也走这里。
- `recolor_html` 返回 `Option<Cow<str>>`：自带深色样式的正文返回借用的原文，不再整份复制；
  `catmail-render` 的 `Recolor` 钩子随之返回 `Cow`，`worker.rs` 的 `recolored` 遇到 `Borrowed` 沿用手上的
  `Arc<str>`。考虑过让钩子直接收发 `Arc<str>`，那样转换要写在 GUI 里，不如 `Cow` 直接。
- 测试：`moves_colors_for_a_dark_page` 增加只在属性里用 `var()`、后面还有普通 `style` 属性的用例，
  自带深色样式的用例改为断言返回 `Borrowed`；`worker.rs` 新增 `a_page_kept_as_it_is_is_not_copied`
  （原样保留时 `Arc::ptr_eq`，未修改前失败）。
