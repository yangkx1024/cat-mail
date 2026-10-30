# 读信区按旧式颜色规则读 bgcolor 和 font color

- 日期：2026-10-30
- 类型：修复

## 背景

issue #161。Blitz 0.3.0-beta.4 的 `parse_color_attr` 只认 `#` 开头的六位或三位十六进制 `bgcolor`，三位的
每位没乘 17：`bgcolor="#fff"` 画成 (15,15,15)，邮件里常见的白底表格成了近黑的一块，深色字看不清。颜色名
（`red`）、省略 `#`（`ff0000`）、前后有空格的写法被忽略；`<font color>` 完全不映射，字一律是黑的。
浅色主题下所有邮件、深色主题下自带深色样式的邮件（`recolor_html` 原样返回）都受影响。

## 决策

- **在 Blitz fork 里修，不在 `catmail-html` 里规范化。** 上游没有相关 issue 或 PR（#662 只改了尺寸属性），
  `main` 上的 `parse_color_attr` 没变。Stylo 自带 HTML 标准 "rules for parsing a legacy colour value" 的
  完整实现（`style::servo::attr::parse_legacy_color`），Blitz 的尺寸属性已经用同一模块的 `parse_length`，
  所以引擎里的修复只是换个函数加一条映射。在我们这边规范化要在读信区打开时多一遍清洗，还要复刻这套规则；
  已有 fork（#164），多一个提交，上游合并后一起去掉。
- 顺带映射 `<body text>`（标准里与 `font color` 同为设置 `color` 的旧属性）。`body link` / `vlink` 要按
  `:link` 状态设色，不在这次范围内。`bgcolor` 适用的元素保持 Blitz 原样（任何元素），不收窄到标准列的
  那几个，免得夹带行为变化。

## 方案

- fork `yangkx1024/blitz` 分支 `catmail/anonymous-cells-in-rows` 新增提交 `ce6f3595`：
  `packages/blitz-dom/src/stylo.rs` 的 `parse_color_attr` 改用 `parse_legacy_color`，`font color` 和
  `body text` 映射为 `color`；`tests/blitz-tests/tests/legacy_color_attributes.rs` 对照 CSS 写法比较计算值
  （没有修复时 `#fff` 和 `font color` 两个测试失败）。
- 根 `Cargo.toml` 的 `[patch.crates-io]` 改到新提交，注释和 `docs/architecture/gui.md` 的"HTML 引擎"一节
  写明第二处改动。
- 测试：`catmail-render` 的 `legacy_color_attributes_are_read_as_a_browser_reads_them`，issue 表里的各种
  写法分别作为 `bgcolor` 和 `font color`（用 `currentcolor` 画成背景以免读字形）取像素，都应是浏览器的颜色；
  旧提交下 `font color` 是黑色，`#fff` 是近黑。
