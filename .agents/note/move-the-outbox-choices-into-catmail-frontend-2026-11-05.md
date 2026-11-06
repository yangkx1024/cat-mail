# 把发件箱右栏的选择抽进 catmail-frontend

- 日期：2026-11-05
- 类型：重构

## 背景

nsh 版（`nsh-spike` 分支）要做发件箱的右栏（ROADMAP P6）。gpui 版 `mailbox/outbox.rs` 里，条目的状态怎么说、
给哪些选择、按什么先后、哪个要先确认、对应哪个引擎调用，都和 UI 库无关，照抄就是两份要一起改的规则。

## 决策

- 状态的文案做成 `State`（未确认、被拒绝、发送中、其他附原因），前端只决定怎么画（gpui 用 `Alert`，nsh 用自己的框）。
- `Choice` 带上 `label`、`confirm`（只有可能重复的 Send again 要确认，给出标题、说明和继续的按钮名）、`what`
  （"Couldn't …" 的说法）和 `run`（对应的 `MailClient` 调用）。gpui 版原来的 `OutboxCommand` 只是 `Choice` 的
  另一份映射，去掉了。

## 方案

- `crates/catmail-frontend/src/outbox.rs`：`State::of`、`Choice`、`choices`。原来的
  `choices_follow_what_the_engine_allows` 测试随代码搬来，新增"只有未确认的重发要先确认"和状态文案的测试。
- gpui 版 `mailbox/outbox.rs` 改成用它，行为不变。
- 顺带：`composer/mod.rs` 一处 import 顺序没过 fmt，一起改了。
