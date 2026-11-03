# 问服务器的搜索提前登录

- 日期：2026-11-03
- 类型：功能

## 背景

评估 #219 时看本地日志：第一次问服务器的搜索，登录占了大头（Gmail 约 1.8 s，QQ 约 0.5 s），比流水线能省的
（Gmail 每个文件夹约 0.4 s）还多。GUI 本地结果显示后等 1 秒（`SERVER_SEARCH_DELAY`）才问服务器，这段时间正好
用来登录。

## 决策

- 引擎新增 `MailClient::connect_for_search(account)`：为搜索会问的账户（`searched_on_server`）登录进
  `SearchSlot`。登录期间一直锁着它，这时开始的搜索等它连好、直接接过去，不另开一条。
- 连接走搜索同样的 `pool::on_session`（已开着的不动，睡眠前建的换掉），有搜索在用（`try_lock` 拿不到）的账户不动；
  登录被拒、超限的记进 `refused`，和搜索自己遇到时一样，不会因为提前连接多一次失败的登录。判断提成
  `Held::may_ask` / `note_refusal`，两边共用。问候语说不能按文字搜的不另记：`connection::connect` 已写进数据库，
  `searched_on_server` 之后不再列出它。
- GUI 只在搜索框获得焦点时调用。考虑过文字变了也调用（防焦点之后隔了 2 分钟才输入）：每次都多查一遍
  `searched_on_server`，而之后的搜索本来就让连接再留 2 分钟，那种少见情况交给搜索自己登录。只在开始输入时连接
  也考虑过：离问服务器只剩约 1.2 s，不够 Gmail 登录。代价是聚焦了却不搜也登录一次，空闲 2 分钟就关，可以接受。
- CLI 的 `search --server` 立刻就问，不用。

## 方案

- `catmail-core/src/search.rs`：`connect_ahead` / `connect_account_ahead`。
- `catmail-core/src/client.rs`：`connect_for_search`。
- `catmail-gui/src/mailbox/mod.rs`：`InputEvent::Focus` 时调用 `connect_for_search`。
- 文档：`storage.md` 问服务器加"提前连接"，`logging.md`。
- 测试：`search.rs` 的 `connecting_ahead_saves_the_search_its_login`（提前连接后搜索不再登录、重复调用不再连）、
  `connecting_ahead_to_a_server_refusing_the_login_keeps_searches_from_trying`。
