# 把写信窗口的模型抽进 catmail-frontend

- 日期：2026-11-05
- 类型：重构

## 背景

nsh 版（`nsh-spike` 分支）要做写信窗口（ROADMAP P5）。gpui 版的写信窗口里，保存、发送、关窗的先后是最容易
写错的部分：同一草稿的引擎调用不能重叠，发送和关窗要等正在进行的保存和附件增删，附件失败要取消等着的发送，
没改过的新草稿关窗时删掉。这些逻辑和 UI 库无关，照抄一份到 nsh 版就要维护两份。

## 决策

- 不只抽纯函数，把整套状态（`edited`、`dirty`、`saving`、`attaching`、`finishing`、`saved`、保存状态）抽成
  `Lifecycle`。窗口把用户做了什么告诉它，它回答下一步（`AfterSave`、`AfterAttachment`、`Closing`、`Steps`），
  窗口按回答调用引擎。这样两个前端只剩界面和各自的任务机制。
- `OpenComposers<W>` 对窗口的类型泛型：gpui 版是 `(AnyWindowHandle, WeakEntity<Composer>)`，nsh 版是 GTK 窗口
  加一个关窗请求的通道。登记、正在打开的草稿、`close_all` 的应答都在它里面。
- 字段读成 `Fields`（几个 `&str`），不碰输入框类型；From 的选项是 `Sender { identity, title: String }`，gpui 版
  再包一层实现 `SelectItem`。
- 新邮件用哪个账户（当前位置的账户，否则第一个）也放进来（`new_message_account`），gpui 版的
  `Mailbox::current_account` 因此不再需要。

## 方案

- `crates/catmail-frontend/src/composer.rs`：`ComposeFrom`（含 `start`：建 / 读草稿和 `draft_senders`，算出
  `created_here`、`focus_body`）、`Opening`、`SAVE_DELAY`、`window_title`、`senders`、`error_text`、`Written`、
  `Field`、`Fields`、`unsendable`、`Finish`、`Lifecycle`、`Closing`、`Steps`（含 `run`）、`OpenComposers`。
- gpui 版 `composer/`：`Composer` 的那些字段换成 `life: Lifecycle`，`lifecycle.rs` 的 `closing`、`steps` 和测试
  搬走，行为不变。
- 测试：`closing` 和 `steps` 的表格测试随代码搬到 frontend；新增保存与发送的先后、一次只存一次、附件失败取消
  发送、没改过的草稿关窗删除、地址解析失败保留存下的值、没有收件人、`close_all` 应答、同一草稿只打开一次。
  gpui 版 `mailbox/tests/writing.rs` 的写信测试照常通过。
