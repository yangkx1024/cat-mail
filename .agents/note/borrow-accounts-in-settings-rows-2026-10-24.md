# 设置 › 账户建行时借用账户，不再深拷贝

- 日期：2026-10-24
- 类型：重构

## 背景

issue #96：`render_accounts` 每次渲染都把每个 `Account` 和它整个 `Vec<Identity>` 深拷贝一遍，每行身份
又把地址 clone 进事件闭包；删除账户、添加身份的闭包各捕获一份整个 `Account`，`AddIdentity` 也存整个
`Account`。设置窗口每次重绘都分配这些，账户和身份越多越多。

## 决策

- 建行时借用 `mailbox.read(cx).accounts`（`cx.listener` 只要 `&self`，与借用不冲突），不改成 `Arc` /
  `SharedString`：那要动 core 的 `Account` / `Identity` 或 GUI 的 `AccountEntry`，收益只差元素里那几个
  字符串。
- 事件闭包只捕获 id，点击时再从 mailbox 查（`SettingsView::account`）；账户此时已不在就什么都不做。
  删除身份对话框里"改用哪个地址发"也在点击时算，原来每次渲染都为它分配一个 `SharedString`。
- `AddIdentity` 只存账户 id 和地址（标题用），显示名只在建表单时用到。

## 方案

- `crates/catmail-gui/src/settings/dialog/accounts.rs`：借用渲染，`confirm_remove_account` /
  `add_identity` / `confirm_remove_identity` 改收 id。
- `crates/catmail-gui/src/settings/dialog/identity.rs`：`AddIdentity::new(client, id, email, name, ..)`。
- 行为不变，没有新测试；现有 GUI 测试（设置里添加账户、身份）照常通过。
