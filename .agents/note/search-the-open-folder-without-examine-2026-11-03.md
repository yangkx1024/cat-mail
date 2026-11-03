# 问服务器的搜索不再重复 EXAMINE 已打开的文件夹

- 日期：2026-11-03
- 类型：功能

## 背景

issue #219 想用命令流水线把每个文件夹的 EXAMINE + UID SEARCH 并成一个来回。评估时看本地日志：Gmail 上
EXAMINE 和 SEARCH 各约 380 ms，几乎全是来回；但流水线要改所有 IMAP 命令共用的 `Connection`（未读回的标签、
乱序完成、出错时保持同步），长期维护成本高，而且只有搜索一个调用方受益（取正文已经不重复 EXAMINE，同步在后台）。
搜索连接上本来就开着上次最后搜的文件夹，再 EXAMINE 一次是白花一个来回。

## 决策

- 先做跳过：搜索连接上已只读打开着的文件夹排到最前，不 EXAMINE。只搜一个文件夹时第二次起就不用 EXAMINE，
  Gmail 两三个位置每次少一个（约 0.4 s）。流水线留在 #219，等这一步之后再看值不值。
- 新增 `Session::examined_mailbox`，不复用 `examined(raw_name, uid)`：后者要求 UID 小于打开时的 UIDNEXT，
  是针对按 UID 取某一封；搜索搜整个文件夹，只要 UIDVALIDITY。
- 服务器还没报告（EXISTS）的新邮件这次搜不到、下次搜得到。会漏的只有这期间被别的连接同步进本地、正文又没下载的，
  接受；为它每次 NOOP 一下就把省下的来回又花回去了。
- 重建的文件夹：服务器断开连接（RFC 3501 §2.3.1.1，假服务器回 BYE），`on_session` 换新连接重新 EXAMINE。
- 两种情况不复用（review 发现）：
  - 服务器上次说是空的：`Session::uid_search` 在 EXISTS 为 0 时不发命令，连接上什么也不发，就永远收不到新邮件的
    EXISTS，之后到的邮件一直搜不到，每次搜索还让连接多留 2 分钟。条件放在 `examined_mailbox` 里：不发命令是 `Session`
    自己的行为。
  - 同步已经记下新的 UIDVALIDITY：拿打开时的旧值比，每次都判为重建、一条命令不发，服务器排着的 BYE 没人读，连接
    换不掉。只在打开时的 UIDVALIDITY 与本地一致时复用，否则 EXAMINE（或读到 BYE 换连接）。

## 方案

- `catmail-proto/src/imap/session.rs`：`examined_mailbox()`。
- `catmail-core/src/search.rs`：`in_folders` 先把已打开的文件夹转到最前；打开着就用记下的 UIDVALIDITY，
  `Searched::Found.examine` 改成 `Option`；`searched the server` 日志加 `opened`（EXAMINE 了几个）。
- 文档：`storage.md` 问服务器、`protocol.md` 连接池、`logging.md`。
- 测试：`imap_session.rs` 的 `examined_mailbox_is_the_one_open_read_only`；`search.rs` 的
  `the_folder_open_already_is_searched_first_without_opening_it`（连续两次搜索只 EXAMINE 一个、限定文件夹的第二次
  一个也不 EXAMINE、文件夹重建后换连接重新 EXAMINE）、`a_folder_open_empty_is_opened_again`、
  `a_folder_open_that_sync_found_rebuilt_is_opened_again`（去掉对应的条件各自失败）。
