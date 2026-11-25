# 表单的主按钮在请求进行中显示转圈

- 日期：2026-11-25
- 类型：功能

## 背景

新建文件夹、标签的对话框点 Create 后要等服务器建好才关，期间只有按钮变淡，看起来像没反应。

## 决策

- `form::form_button` 的主按钮（填色的那个）在 `busy` 时用 macOS 的转圈（`buttons::spinner`，14 pt，
  `primary_foreground`）替代文字：文字还在、透明，按钮宽度不变，旁边的 Cancel 不会挪动。不再变淡，同
  `busy_icon_button`："不变淡，转圈说明了原因"。
- 所有用 `form_button` 的表单（命名对话框、登录、身份、外观的保存等）都跟着有了。

## 方案

- `crates/catmail-gui/src/form.rs`：`form_button`。
- 没加测试：转圈是纯视觉，busy 期间按钮关掉的行为不变，现有测试覆盖。需要在运行中的应用里看一眼。
