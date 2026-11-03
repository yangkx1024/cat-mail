# 连接时记下服务器能否按文字搜

- 日期：2026-11-03
- 类型：功能

## 背景

#223。Coremail（网易 163、126 等）的 SEARCH 除 Message-ID 外搜不到文字，#224 按问候语认出它，但只记在
内存里的 `SearchSlot`：每次引擎运行，第一次问服务器的搜索仍要为它连接、读 keyring、登录（163 上 2～4 秒）、
EXAMINE 第一个文件夹才知道。`search_server` 等所有账户搜完才返回，这次登录也推迟了其他账户的补充结果。

## 决策

- 在 `connection::connect` 里记，而不是只在同步里记：所有 IMAP 连接都走它（同步、连接池、预取、发送），谁先连上
  谁记；`add_account` 用 `Verify::Login` 时也记。和 `imap_append_limit`、`gmail_server` 一样，有变化才写。
- 存进 `accounts.searches_text`（`0001_init.sql` 原地修改），不放引擎内存：重启后、第一次同步完成前的搜索，以及
  CLI 一次性的 `catmail search --server` 都不用连接。
- 放弃拆开连接与登录：要改所有连接共用的入口，只为搜索一个调用方。
- 对照搜索发现的"无视文字"仍只记在 `SearchSlot`：不是问候语认出来的，可能是偶然，不持久化。
- 不放进 `Account`：只有引擎用它，前端不需要。

## 方案

- `catmail-store`：新列 `searches_text`，`Accounts::searches_text` / `set_searches_text`；
  `Folders::searched_on_server` 只列出 `searches_text` 的账户。
- `catmail-core`：`connection::Announced` 收拢服务器自己说明的几项（APPENDLIMIT、是否 Gmail、能否按文字搜），
  `connect` 与记录的比较、有变化才写，`add_account` 用 `Verify::Login` 时记下验证连接的同一份；以后新增按服务器
  区分的项只加在它里面。
- 测试：`tests/search.rs` 的 `a_server_greeting_that_it_cant_search_text_isnt_logged_in_to`：Coremail 问候语下
  添加（`Verify::Login`）并同步后，搜索不发任何命令、不增加连接，重开引擎后也一样；问候语恢复正常、再同步后
  照常搜索。去掉 `searched_on_server` 的过滤时这个测试失败（出现 LOGIN、EXAMINE）。
  `accounts.rs` 单元测试覆盖读写。
