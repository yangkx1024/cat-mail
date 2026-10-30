# 自动表格布局里把单元格宽度当作最小宽度

- 日期：2026-10-30
- 类型：修复

## 背景

GitHub Actions 的通知邮件里，任务表格的表头 "Status" 和 "Job" 叠在一起，读信区显示成 "Statobs"。
表头是 `<th style="width: 24px; box-sizing: border-box; padding-left: 16px">Status</th>`，下一列
`width: 100%`。浏览器的自动表格布局里，长度 `width` 只是列的最小宽度，列会撑到单元格内容的 min-content
宽度（约 66px）。Blitz 0.3.0-beta.4 把第一行单元格（或 `<col>`）的长度宽度直接做成固定宽度的网格轨道，
内容比它宽就溢出到下一列。上游 `main` 的 `table.rs` 这部分没变，也没有相关 issue 或 PR；修复已作为
DioxusLabs/blitz#1345（draft）提给上游。

## 决策

- **在 Blitz fork 里修。** 清洗器改不了：要知道内容的 min-content 宽度才能定列宽，只有排版时知道。
  fork 已有两处改动，多一个提交。
- **按 CSS Tables 3 的语义，而不是只让列不小于内容。** 有长度宽度的列（constrained column）最小宽度是
  max(宽度, 本列单元格的 min-content)，最大宽度也一样，不会因为有剩余空间就按 max-content 变宽，多余的
  宽度给自动列；跨列单元格多出来的宽度也只给自动列。
- **保留固定轨道，排版前加宽。** 先试过把这种列改成 `minmax(min-content, max-content)` 轨道：上游 WPT
  `css/css-tables` 多过 13 个，但新挂 3 个（`computing-row-measure-1`、`colspan-redistribution` 表 18、25），
  因为 Taffy 把跨列单元格的宽度也分给了内在尺寸轨道。现在轨道仍是固定长度，`TableTreeWrapper::new` 在跑
  网格之前量本列单跨单元格的 min-content，取最大值（不小于列宽）作为轨道长度，跨列分配和原来一样。WPT
  多过 9 个子测试和 1 个测试，没有新失败。
- 单元格自己的 `width` 在量内容时要忽略（否则 Taffy 按 `width` 定死成 24px），用 `SizingMode::ContentSize`；
  匿名单元格（行里的块）是盒子本身，它的 `width` 就是内容的宽度，保留原样。
- 只有跨列单元格或没有单元格的列不加宽。固定表格布局不变。
- 仍沿用 Blitz 只看第一行单元格定列宽的做法，后面行里单元格自己的长度宽度作为它自己的下限。

## 方案

- fork `yangkx1024/blitz` 分支 `catmail/anonymous-cells-in-rows` 新增提交 `7333936c`（`minmax` 版本）和
  `046129ea`（改为固定轨道加宽，同上游 PR）：`packages/blitz-dom/src/layout/table.rs` 给 `TableCell` 加
  `min_width`，`build_table_context` 在自动布局里记下这些列（`minimum_columns`）并把宽度交给列里的单元格，
  `TableTreeWrapper::compute_child_layout` 在宽度未知时按上面的方式量，`widen_columns` 在排版前加宽轨道；
  `tests/blitz-tests/tests/table_cell_width.rs` 五个测试（内容更宽时撑开、内容更窄时保持宽度、可折行内容
  不按 max-content 变宽、后面行的单元格也受列宽下限约束、跨列单元格不加宽受约束的列）。没有修复时要撑开的
  两个失败，跨列那个在 `minmax` 版本下失败。
- 根 `Cargo.toml` 的 `[patch.crates-io]` 改到新提交，注释和 `docs/architecture/gui.md` 的"HTML 引擎"一节
  写明第三处改动。
- 测试：`catmail-render` 的 `a_column_grows_past_its_width_to_fit_its_content`，24px 宽的列里放 50px 的块，
  下一列的内容应从 50px 开始；旧提交下下一列压在第一列上。另外用这封邮件存库的 HTML 渲染确认表头排成
  "Status / Job / Annotations"。
