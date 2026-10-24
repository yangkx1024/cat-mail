# 确认框焦点在 Cancel 上按 Enter 不再执行确认

- 日期：2026-10-24
- 类型：修复

## 背景

issue #117：`layout::confirm_danger` 的确认框（删除账户、删除发件地址、永久删除邮件等）里，Tab 到 Cancel
再按 Enter，执行的却是确认。gpui-base 的 Dialog 在 `Dialog` 键上下文里把 `enter` 绑成 `Confirm`，按键按下时
就被分派，按钮的键盘点击要等抬起才触发，所以 `AlertDialog` 自动生成的 Cancel 按钮按 Enter 也等于确认。

## 决策

- 沿用添加发件地址对话框（#99）的做法：`confirm_danger` 通过 `AlertDialog::footer` 给出自己的 Cancel 和
  确认按钮，带 `DialogButton` 键上下文（`actions.rs` 在那里把 Enter 绑成 `NoAction`），点击时派发对话框的
  `Cancel` / `Confirm`。`AlertDialog` 生成的按钮加不上键上下文，只能换掉。
- 根本修复应在 GPUI Kit：Dialog 在焦点位于按钮上时不拦截 Enter。上游修好前先在本地绕过。

## 方案

- `layout.rs`：`confirm_danger` 自己给 footer；按钮 id 改为 `CONFIRM`（`confirm-ok`）和新增的 `CANCEL`
  （`confirm-cancel`），不再依赖 gpui-kit 的 `ok`。
- `docs/architecture/gui.md`、`testing.md` 相应更新。

验证：新增 `enter_on_cancel_of_a_confirmation_removes_nothing`（Settings › Accounts 删除账户，Tab 到 Cancel
按 Enter：对话框关闭、账户还在；再 Tab 到 Remove 按 Enter：账户被删除）。去掉 Cancel 按钮上的键上下文，
测试失败（账户被删）。
