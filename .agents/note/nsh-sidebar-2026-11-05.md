# nsh 版侧栏

- 日期：2026-11-05
- 类型：功能

## 背景

catmail-nsh 对齐 gpui 版的 P2（`crates/catmail-nsh/ROADMAP.md`）。原来的侧栏把每个账户的文件夹按名字平铺，
没有角色排序、All Inboxes、草稿、发件箱、Labels、连接说明和右键菜单；收到事件就整个重读。这些规则在 gpui 版里
大多是与窗口无关的纯逻辑，已抽进 `catmail-frontend` 共用。

## 决策

- 侧栏的模型与 gpui 版共用，在 `catmail-frontend`（见 `extract-sidebar-model-into-catmail-frontend-2026-11-05.md`）。
- **nsh 版只写界面**（`sidebar.rs`）：把模型拍平成行（标题、账户、提示、位置），用 `each` 按 key 渲染；
  右键菜单用原生的 `popup_menu`，删除确认用原生的 `alert_dialog`，命名用 `dialog()` 加 `input()`。
  nsh-base 也有无样式的 `ContextMenu` 组件，但原生菜单更像桌面应用，也省得自己画。
- **引擎事件**按 `Refresh::after` 决定重读侧栏还是列表，`Throttle` 节流、`Coalesce` 合并读侧栏，代替原来的
  "250 ms 后全部重读"。Drafts、Outbox 的列表属于 P3 的来源，现在选中时列表是空的。
- **由行发起、要比行活得久的任务**（同步打开的文件夹、删除、读侧栏）挂在邮箱视图的 `Owner` 下
  （`app::in_mailbox`）：nsh 的 `spawn` 归当前 owner，挂在行下时行一消失任务就被取消。
- nsh 的主题没有 warning 色，发件箱的警告图标先用 destructive 色。
- 子文件夹的折叠、底部的 Settings / Sync 按钮没做，roadmap 里列为未完成。

## 方案

- nsh：新增 `src/sidebar.rs`、`src/icons.rs`（`include_str!` gpui 版的 Heroicons，`svg` 画，`currentColor`
  跟文字色）；`src/app.rs` 的选中从文件夹换成 `Place`，事件处理改为 `on_event`。
- 新增调试变量 `CATMAIL_NSH_UNFOLD`：展开所有账户。这台机器没有能模拟点击的工具，截图要靠它。
- 验证：nsh 版 clippy 通过。真实数据目录截图：4 个账户时有
  All Inboxes 并默认选中、账户默认折叠；展开后 QQ 的角色文件夹排前、Drafts 在 Inbox 后、子文件夹缩进、
  Gmail 的 Labels 单列、Sent / Junk / Trash 不显示未读数。右键菜单（账户和文件夹）、新建、重命名、删除和 F2
  由用户手动试过（这台机器没有模拟输入的工具）。
