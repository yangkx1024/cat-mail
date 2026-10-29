# 按需下载不再重复 EXAMINE，连接池里的连接保持可用

- 日期：2026-10-29
- 类型：功能

## 背景

issue #156：在 Gmail 上打开一封没下载正文的邮件，几乎全部时间花在和服务器的往返上。第一封要新建连接
并登录（约 1.2 s），之后每封都先 EXAMINE All Mail（约 0.5 s）再 FETCH（约 0.6 s）。池里的连接闲置
5 分钟（`IDLE_SESSION_TTL`）就不再复用，隔一会儿再打开邮件又要重新登录；过期的连接也要等下次有人来取
才关掉。

## 决策

- **A：同一个文件夹不再 EXAMINE。** 只在连接用 EXAMINE 打开着同一个文件夹、UIDVALIDITY 与本地一致时
  跳过。用 SELECT 打开的不复用：`BODY.PEEK` 本身不改动什么，但"读取只在只读打开的文件夹上做"这条规则
  更简单、更稳，代价只是一次 replay 之后多一次 EXAMINE。只改按需下载；同步、草稿上传仍每次 EXAMINE，
  它们要最新的 EXISTS / UIDNEXT / HIGHESTMODSEQ。
- 以前文档说"复用的连接仍然每次 EXAMINE"，理由是要防服务器在选中期间改 UIDVALIDITY。RFC 3501 规定
  打开期间 UIDVALIDITY 不变，重建选中的文件夹时 Dovecot 直接回 BYE 断开；复用的连接断开后会换新连接
  重新 EXAMINE，照样报 `OutOfSync`。假服务器原来只在 IDLE 中这样做，现在任何命令都一样。
- **B：后台 keepalive，而不是取连接时先 NOOP。** 取的时候 NOOP 要让用户多等一个往返，正好抵掉 A 省下的；
  后台每 4 分钟 NOOP 一次不在用户路径上。每条放回池里的连接起一个任务（按 id 认领，被取走就结束），
  比每账户一个调度任务简单。去掉了 TTL：连接靠 keepalive 续着，死了就当场关掉，"过期连接不及时关"的
  问题随之消失。间隔 4 分钟放进 `EngineTuning::keepalive`，测试用 50 ms。
- 保活不设闲置上限（比如 30 分钟没人用就关掉）：程序还在运行（包括关了窗口、留在托盘里），就是要随时
  能打开邮件，连接就该一直可用；代价是每条池连接每天约 360 次 NOOP，可以接受。
- 连接池拆成独立模块（`pool.rs`）留给 #158，这次不混进功能改动。
- 没有选预取正文（会改变按需下载的设计）。

## 方案

- `catmail-proto`：`Session` 记下打开的文件夹名（`selected: Option<(String, MailboxStatus)>`），新增
  `examined(raw_name)`（只读打开着它时返回它的 UIDVALIDITY，其余状态可能已过时，不给出）和 `noop()`。
- `catmail-core/src/body.rs`：`examine` 先看 `session.examined`，一致就跳过；`fetched from the server`
  日志里 `examine` 变成 `Option`，跳过时为 `None`。
- `catmail-core/src/engine.rs`：`IdleSession` 用 `id` 代替 `since`；`return_session` 起 `keep_alive`
  任务（弱引用引擎、监听 `closing`），睡前建立的连接直接关掉，NOOP 遇到致命错误就关掉，否则放回。
- `catmail-testkit`：所选文件夹 UIDVALIDITY 变了，下一条命令回 BYE 断开。
- 测试：`bodies.rs` 新增同一文件夹第二封不 EXAMINE（SELECT 之后又 EXAMINE）、连接经过多次 keepalive
  仍回到池里、不重新登录（只看服务器状态：在这之后再下载会和进行中的 NOOP 抢连接，结果不确定）、服务器断开的池中连接被 keepalive 发现并换掉；`imap_session.rs` 新增 `examined`
  只认只读打开的那个文件夹。已有的 UIDVALIDITY 变化测试照常通过（走断线重连再 EXAMINE）。
- 文档：`protocol.md` 连接池、`bodies.md`、`background.md`、`testing.md`。
