# 自动表格布局里把单元格宽度当作最小宽度

- 日期：2026-10-30
- 类型：修复

## 背景

GitHub Actions 的通知邮件里，任务表格的表头 "Status" 和 "Job" 叠在一起，读信区显示成 "Statobs"。
表头是 `<th style="width: 24px; box-sizing: border-box; padding-left: 16px">Status</th>`，下一列
`width: 100%`。浏览器的自动表格布局里，长度 `width` 只是列的最小宽度，列会撑到单元格内容的 min-content
宽度（约 66px）。Blitz 0.3.0-beta.4 把第一行单元格（或 `<col>`）的长度宽度直接做成固定宽度的网格轨道，
内容比它宽就溢出到下一列。上游 `main` 的 `table.rs` 这部分没变，也没有相关 issue 或 PR。

## 决策

- **在 Blitz fork 里修。** 清洗器改不了：要知道内容的 min-content 宽度才能定列宽，只有排版时知道。
  fork 已有两处改动，多一个提交。
- **按 CSS Tables 3 的语义，而不是只让列不小于内容。** 有长度宽度的列（constrained column）最小宽度是
  max(宽度, 各单元格 min-content)，最大宽度也一样，不会因为有剩余空间就按 max-content 变宽，多余的宽度
  给自动列。所以轨道是 `minmax(min-content, max-content)`，同时这一列的单元格量内在宽度时两种都按
  min-content 量，再和宽度取大。
- 单元格自己的 `width` 在量内容时要忽略（否则 Taffy 按 `width` 定死成 24px），用 `SizingMode::ContentSize`；
  匿名单元格（行里的块）是盒子本身，它的 `width` 就是内容的宽度，保留原样。
- 只有跨列单元格或没有单元格的列保留固定轨道，免得 `<col width>` 撑开的空列变成 0。固定表格布局不变。
- 仍沿用 Blitz 只看第一行单元格定列宽的做法，后面行里单元格自己的长度宽度作为它自己的下限。

## 方案

- fork `yangkx1024/blitz` 分支 `catmail/anonymous-cells-in-rows` 新增提交 `7333936c`：
  `packages/blitz-dom/src/layout/table.rs` 给 `TableCell` 加 `min_width`，`build_table_context` 在自动布局里
  把长度宽度的列换成 `minmax(min-content, max-content)` 并把宽度交给列里的单元格，`TableTreeWrapper::
  compute_child_layout` 在宽度未知时按上面的方式量；`tests/blitz-tests/tests/table_cell_width.rs` 四个测试
  （内容更宽时撑开、内容更窄时保持宽度、可折行内容不按 max-content 变宽、后面行的单元格也受列宽下限约束），
  没有修复时要撑开的第一和第四个失败。
- 根 `Cargo.toml` 的 `[patch.crates-io]` 改到新提交，注释和 `docs/architecture/gui.md` 的"HTML 引擎"一节
  写明第三处改动。
- 测试：`catmail-render` 的 `a_column_grows_past_its_width_to_fit_its_content`，24px 宽的列里放 50px 的块，
  下一列的内容应从 50px 开始；旧提交下下一列压在第一列上。另外用这封邮件存库的 HTML 渲染确认表头排成
  "Status / Job / Annotations"。
