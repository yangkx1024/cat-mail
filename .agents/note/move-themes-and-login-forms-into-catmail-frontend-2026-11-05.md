# 把主题和登录表单的逻辑抽进 catmail-frontend

- 日期：2026-11-05
- 类型：重构

## 背景

nsh 版（`nsh-spike` 分支）要做设置对话框和登录页（ROADMAP P7、P8）。gpui 版里主题的推导（终端调色板 → 全部颜色）、
十来种终端配色的导入、主题目录的读写，以及登录表单怎么把字段读成账户 / 发件身份，都和 UI 库无关，照抄一份就要
维护两份。

## 决策

- 主题在 frontend 里是自己的 `ThemeConfig` / `ThemeSet`：只关心名字、明暗和按 gpui-kit 键名的颜色，其余字段用
  `serde(flatten)` 原样保留。文件格式仍是 gpui-kit 主题 JSON，两个前端读写同一个主题目录，已有的文件照常能读。
  考虑过定义一套中性的颜色角色再各自展开，但那要换文件格式、迁移用户已导入的主题，收益只是名字好看。
- GUI 经 serde 把 frontend 的主题转成 gpui-kit 的 `ThemeConfig`；转不了的（手改坏的文件）记警告后不提供，和
  原来"读不了的文件跳过"一致。导入 gpui-kit 主题时只检查颜色都是字符串，其余结构的校验留给转换。
- 只依赖 gpui-kit 才有意义的两个测试（每个颜色键都设了、经 gpui-kit 往返）留在 GUI 的 `appearance.rs`。
- 主题卡片的色块（`Swatch`）仍在各前端：颜色怎么解析是 UI 库的事。
- 登录表单：`LoginValues`、`ServerValues`、`AccountValues::new_account`、`IdentityValues::new_identity`、
  安全选项和两句说明搬进 `login`；提交的状态（进行中不重复提交、值不对或请求失败时的错误）抽成
  `Submission`，GUI 的 `Submit` 只剩任务和按钮样式。
- Settings › Accounts 的确认文案、改名和添加地址的标题、新地址预填的名字搬进 `accounts`；读信设置里远程图片
  缓存的说明搬进 `settings::image_cache_help`。`commands::Confirm::description` 改成 `String`，因为删除发件身份
  的说明里有默认地址。

## 方案

- `crates/catmail-frontend/src/theme/`：`palette.rs`、`builtin.rs`、`derive.rs`、`import/` 从 GUI 搬来
  （`git mv`），`mod.rs` 定义 `ThemeConfig`、`ThemeSet`、`ThemeMode`；`themes.rs` 是原 `appearance.rs` 里与 GPUI
  无关的部分（`Themes::load` / `pick` / `install`、文件命名、`read_import`、导入的标题和说明文字）。
  `toml`、`quick-xml` 依赖随之从 GUI 移到 frontend。
- `crates/catmail-frontend/src/login.rs`、`accounts.rs`。
- GUI：`theme/appearance.rs` 包着 frontend 的 `Themes`，换主题后重新转换；`login_form.rs`、
  `mailbox/add_account.rs`、`settings/dialog/{identity,accounts,themes,reading}.rs`、`submission.rs` 改用
  frontend 的类型和文案，行为不变。
- 验证：原有的推导、导入、主题目录、登录表单测试随代码搬到 frontend（新增内置主题名不被占用、
  `Submission` 一次一个）；GUI 的设置、写信等集成测试照常通过。
