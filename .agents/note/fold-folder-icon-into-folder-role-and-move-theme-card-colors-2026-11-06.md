# FolderIcon 复用 FolderRole、加 Place::icon，主题卡片的颜色回退移进 frontend

- 日期：2026-11-06
- 类型：重构

## 背景

#245：`FolderIcon` 把 `FolderRole` 的九个变体照抄了一遍，新增一个角色要在 types、frontend、gui 各改一处以上；
侧栏建 All Inboxes、Drafts、Outbox 的行时手写名字和图标，和列表标题用的 `Place::label` 可能对不上；主题卡片的
颜色回退规则写在 gui 的 `theme_card` 里，对着 `Hsla` 写、没有测试。

## 决策

- `FolderIcon::Role(FolderRole)` 加上 `Label`、`Folder`、`Inboxes`、`Outbox`。`RoleInfo` 只剩排序，改成私有的
  `role_order`。
- `Place::icon(accounts)` 和 `Place::label` 并列。侧栏里不是文件夹树的行（All Inboxes、Drafts、Outbox）只给
  `Place`，名字和图标都从它来；文件夹树的行仍用节点自己的名字和图标，免得每行再按 id 找一遍节点。
- `theme::Swatch::of(&ThemeConfig)` 算出卡片要的颜色。颜色用新的 `theme::Rgba`（`Rgb` 加 alpha）：原来对着
  `Hsla` 乘不透明度，`#rrggbbaa` 的 alpha 也要留着。读不了的颜色和没设一样走回退；nsh 的 `Color::parse` 还认
  CSS 的 `rgb()` 和颜色名，主题文件不这样写（导入、推导出的都是十六进制），不另外支持。

## 方案

- `catmail-frontend`：`folders.rs`（`FolderIcon`、`role_order`）、`mailbox.rs`（`Place::icon`，测试名字和图标）、
  `theme/swatch.rs`（`Rgba`、`Swatch` 和表驱动测试）；`Rgb::WHITE` 不再只在测试里。测试用的账户、文件夹、邮件
  放进 `fixtures.rs`，后面几个模型的测试共用。
- `catmail-gui`：`icons.rs` 的表按 `FolderRole` 展开；`settings/appearance.rs` 的 `theme_card` 只把 `Rgba` 换成
  `Hsla`；`mailbox/sidebar.rs` 的 `Item::of` 从 `Place` 取名字和图标。
