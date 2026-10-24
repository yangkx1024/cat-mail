# 添加发件地址的表单开成独立对话框

- 日期：2026-10-24
- 类型：重构

## 背景

issue #99：Settings › Accounts 的"Add sending address"表单原地替换账户列表（`SettingsView::identity_form`）。
为了不让字段里的 Enter 触发设置对话框的 `Confirm` 关掉整个设置，设置的 `on_ok` 被改成不关闭，表单再用
`login_form::submit_on_enter` 自己处理 Enter：两套 Enter 机制，`SettingsView` 多一个表单状态，而 Esc
仍归设置对话框，表单打开时按 Esc 会关掉整个设置（新测试在旧代码上复现了这一点）。

## 决策

- 表单用 `window.open_dialog` 叠在设置上面：`on_ok` 提交并返回 false，成功后表单自己 `close_dialog`；
  Enter（输入框冒泡的 `Confirm`）、Esc、Cancel、关闭按钮都走对话框的约定，嵌套的浮层从最上层关。
- 提交进行中 `on_cancel` 返回 false，Esc、Cancel、遮罩和关闭按钮都关不掉，与登录页"进行中 Cancel 不可用"
  的理由一致（引擎照样会存下身份）。
- 按钮放进 `DialogFooter`，`on_click` 直接写在按钮上，派发对话框的 `Cancel` / `Confirm`。最初用
  `DialogClose` / `DialogAction` 包着按钮：点击处理在包装层，按钮本身没有，Tab 到按钮后空格无效；
  而且这两个包装撑满自己的盒子，会把按钮拉成各占一半。GPUI Kit 自己的默认按钮用的派发锚点
  （`DialogDispatchAnchor`）是 crate 私有的，用不上；焦点被困在最上层对话框里，按焦点派发就能到达它。
- 对话框把 Enter 绑成 `Confirm`，按下时就被这个绑定拿走，按钮等不到按键抬起时的键盘点击：Tab 到
  Cancel 按 Enter 反而提交。按钮带 `DialogButton` 键上下文，`actions.rs` 在那里把 Enter 绑成
  `NoAction`，更深的上下文压过对话框的绑定。GPUI Kit 自己的 `AlertDialog` 按钮也有这个问题，见下面
  另开的 issue。
- `LoginFields::new` 不再自己聚焦，改由 `LoginFields::focus` 调用：对话框打开时记下当前焦点、关闭时还回去，
  表单若在打开前就聚焦 Email，关闭后焦点落在已消失的字段上，再按 Esc 关不掉设置。
- `Submit::actions` 拆出 `cancel_button` / `submit_button` / `busy`，对话框的 footer 复用同样的样式；
  `Submit::start` 的完成回调多传 `Window`，表单才能关掉自己的对话框。
- 登录页不是对话框，`submit_on_enter` 留给它用。

## 方案

- `settings/dialog/identity.rs`：`identity::open` 打开对话框；表单不再有标题行、按钮行和
  `IdentityFormClosed` 事件。
- `settings/dialog/mod.rs`、`accounts.rs`：去掉 `identity_form` 状态和设置对话框的 `on_ok` 覆盖。
- `login_form.rs`、`mailbox/add_account.rs`：上面 `Submit` 和 `LoginFields::focus` 的改动。
- `actions.rs`：`DIALOG_BUTTON` 键上下文和 Enter 的 `NoAction` 绑定。
- `docs/architecture/gui.md` 更新登录页一节。

验证：新增 `escape_and_cancel_close_only_the_sending_address_form`（Esc 和 Cancel 只关表单、设置仍在、
没有新增身份；旧代码上 Esc 关掉了整个设置，测试失败）；新增 `the_sending_address_form_works_from_the_keyboard`
（Tab 到 Cancel 按 Enter 取消、Tab 到 Add 按空格提交、关掉表单后再按 Esc 关掉设置；分别去掉 `NoAction`
绑定、按钮上的 `on_click`、打开后再聚焦，测试都会失败）；`adds_a_sending_address_and_makes_it_the_default`
仍靠字段里按 Enter 添加。截图测试加了 `settings-add-identity`，确认按钮布局。
