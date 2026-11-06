# 拆出 catmail-frontend 之后的收尾清理

- 日期：2026-11-05
- 类型：重构

## 背景

把桌面逻辑搬进 `catmail-frontend` 之后，从复用、简化、效率和层次四个角度看了一遍整个分支，发现搬完留下的一些尾巴：
同一条规则在 frontend 和 gui 各有一份、只剩一个调用方的包装、外部没人用的公开 API、gui 里已经不再用的依赖，
以及搬家时多出来的几处小开销。

## 决策

- 只改在这个分支范围内、行为不变的部分。更大的结构调整另开 issue（#241–#245）：把列表加载、侧栏重载、命令执行、托盘计数和绘制
  的协调逻辑也搬进 frontend，以及 `FolderIcon` / `Place::icon`。
- gui 里转发 frontend 类型的 `pub use` 保留，这是之前有意定下的路径。`data_url`、`Pixmap::from_rgba` 是给 nsh
  分支用的，也保留。
- gui 的 `ThemeEntry` / `Swatch` 没动：它们缓存 GPUI 的转换结果，还要跳过 GPUI Kit 读不了的主题。
- `Lifecycle::begin_save` 没改成接收闭包：gui 那边同时借用 `self.life` 和 `self` 会冲突，所以另加了
  `save_due()`，先查它再去读字段。

## 方案

- All Inboxes 的未读数改用 `Place::Inboxes.folders`，删掉 gui 里的 `Mailbox::inboxes()`，侧栏和列表标题共用
  一份定义。
- 新邮件通知用 `commands::one_account` 判断是否来自同一个账户。
- `NotTls` 的提示改为取 `Security::Tls.default_imap_port()` / `default_smtp_port()`，不再写死 993 和 465。
  `net::connect` 里塞在 `Box::new` 中的 `map_err` 拆了出来。
- gui：
  - 新增 `Mailbox::account_folders`，替换三处一样的闭包。
  - 删掉 `#[cfg(test)] can_archive` 和 `connection_note` 包装。
  - `changeable_folder` 只返回 `&Folder`。
  - 删掉不再使用的 `jiff`、`thiserror`，`tempfile` 移到 dev-dependencies。
- frontend：
  - 删掉没人用的 `SettingsFile::dir()`。
  - `theme_config`、`import`、`ImportError` 改为 crate 内部可见。
  - 新增 `composer::discard_confirm()`，"Discard this draft?" 的文案从 gui 移过来。
- 效率：
  - `save()` 先查 `save_due()` 再读草稿。
  - `picker_filter` 把查询词转小写只做一次。
  - `Lifecycle::status` 返回 `Cow<'static, str>`。
  - 日期分组标题和提交错误不再先复制一份 `String`。
- 验证：
  - `cargo fmt`、`clippy -D warnings`、全量测试和 `cargo deny` 都通过。
  - `the_picker_filters_by_name` 和 composer 的保存测试随签名一起改了。
