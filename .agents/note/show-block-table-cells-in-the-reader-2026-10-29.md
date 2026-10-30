# 读信区显示改成块的表格单元格

- 日期：2026-10-29
- 类型：修复

## 背景

iCondo 的邮件（Constant Contact 模板）在读信区只显示图片和按钮，正文文字全部不见，在浏览器打开正常。
文字都在 `<td style="display: block">` 里。Blitz 0.3.0-beta.4 的 `collect_table_cells` 把表格行里
`display` 为 block / flow-root / flex / grid 的子元素当作"大概是标题"直接忽略，不排版也不绘制。浏览器按
CSS 2.2 §17.2.1 给它们套一个匿名单元格。Mailchimp 模板在样式表里写 `td.container { display: block }`，
整封邮件空白，问题相同。

## 决策

- **在 Blitz 里修，不在清洗时改写。** 清洗器只看得到内联 `style`：样式表规则要做选择器匹配和层叠，
  媒体查询（如 `@media (max-width: 480px) { td.OneColumnMobile { display: block !important } }`）还取决于
  读信区宽度，存库时的一次清洗无从知道。把被包的 `td` 改成 `div` 还会让 `td.x`、`tr > td` 选择器失效。
  Blitz 布局时拿到的是计算后的 `display`，这些情况都能正确处理。
- **只移植 #781 的思路，不用 #783。** 上游有未合并的 #781（每个子元素当作一个单元格）和 #783（完整的
  匿名盒子修正，+368/-199）。#781 在 beta.4 上 3 个 hunk 失败 2 个，所以按同样的思路重写，大约 20 行。
  代价是一行里连续几个块级子元素各占一列，浏览器会让它们共用一个单元格竖着叠。
- 只处理父节点就是当前行的元素，不碰行外面游离的块（`rows` 为空时会越界）。`display: table-caption`
  的 `inside()` 也是 `Flow`，要排除掉（#783 修过 #781 的这个回归）。被当成单元格的盒子不读
  `colspan` / `rowspan`，border-collapse 下保留自己的边框，保留外边距（`margin: 0 auto` 居中）。
- 同一个 workspace 里用到的 6 个 crate（blitz-dom/html/paint/traits、stylo_taffy、debug_timer）都从 fork
  取：只 patch blitz-dom 会通过路径依赖引入第二份 blitz-traits，类型对不上。

## 方案

- fork `yangkx1024/blitz` 的 `catmail/anonymous-cells-in-rows` 分支，基于 `v0.3.0-beta.4` 的一个提交，
  改 `packages/blitz-dom/src/layout/table.rs`。
- 根 `Cargo.toml` 的 `[patch.crates-io]` 按提交固定 fork；`deny.toml` 的 `allow-git` 加上 fork；
  `docs/architecture/gui.md` 的"HTML 引擎"一节说明 fork 和回到 crates.io 的条件。
- 测试：`catmail-render` 的 `a_cell_made_a_block_is_laid_out_as_a_cell`，内联和样式表两种写法都应选到
  文字；没有这个补丁时只得到空串。另外用这封邮件的 handoff HTML 临时验证，正文、"What's next?"、
  署名、页脚都排出来了。
