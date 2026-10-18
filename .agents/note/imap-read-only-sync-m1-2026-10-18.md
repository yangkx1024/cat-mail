# M1：IMAP 只读同步、认证接口与协议测试环境

- 日期：2026-10-18
- 类型：功能

## 背景

M0 只有账户管理，不连服务器。M1 要能把文件夹和邮件头同步到本地并在命令行列出，同时把协议
测试环境建起来，让断线、UIDVALIDITY 改变、乱序响应这类问题从一开始就有测试覆盖。

## 决策

- **自己驱动 imap-next 的 sans-IO 状态机**，不用它自带的 `Stream`（只支持 `TcpStream`）。
  泛型的字节流让 STARTTLS 升级、内存测试都很直接；升级时连同旧客户端一起丢弃缓冲，避免
  STARTTLS 明文注入。
- **只读同步用 EXAMINE**，不改服务器状态。
- **游标只代表一次完整同步**，不再按 M0 设想的"每批与游标同一事务"。原因：新邮件从新到旧
  拉取（界面先看到最新的），UIDNEXT 无法按批推进；而已入库的邮件行本身就是进度，中断后
  重新求 UID 差集即可续传。快速路径：UIDNEXT 与条数都没变就跳过 SEARCH，HIGHESTMODSEQ 没变
  就跳过 flags。没有 QRESYNC 也能工作，QRESYNC/VANISHED 留作优化。
- **认证接口是 `Credentials` 枚举 + `auth.rs` 里按 `AuthKind` 取凭据**，而不是 trait 对象：
  OAuth2 只需加变体和一个分支。dyn async trait 在这里没有带来额外的灵活性。
- **TLS 用 rustls + ring + 平台校验器**：放弃 aws-lc-rs（Windows CI 需要 cmake/NASM）和
  webpki-roots（不认企业内部 CA）。
- **文件夹名**同时存服务器原名 `remote_name`（命令用）和解码后的 `name`（显示用），避免
  修改版 UTF-7 编解码不可逆时发错命令。
- **邮件头用 mail-parser 解析**（`full_encoding`，覆盖 GBK/GB2312/Big5），只拉
  `HEADER.FIELDS`，不拉整个头部。
- **协议测试环境**：进程内假 IMAP 服务器（新 crate `catmail-testkit`，按字节写响应，故障可以
  精确到线上格式，三平台 CI 都能跑）+ 用 Nix 提供的真实 Dovecot。本机没有 docker，CI 的
  macOS/Windows 也跑不了 docker 服务，所以放弃 README 原先写的 docker 方案；Dovecot 以普通
  用户运行，版本随 flake.lock 固定。
- **账户删除取消同步**：每账户一把锁 + watch 取消信号，删除时不必等网络超时。完整的
  per-account actor 留到 M4（IDLE）。
- **计数由触发器维护**：`messages` 的插入、删除、`\Seen` 变化各 O(1) 更新文件夹计数，取代最初每批
  全表重算（10 万封的文件夹首次同步约 2500 万行扫描），也不再依赖每个写入方记得刷新。
- **领域类型共享**：`FlagState` 放在 `catmail-types`，proto 与 store 共用，`NewMessage` /
  `FetchedMessage` 内嵌它。用户可处理的错误（认证失败、缺少 STARTTLS）在 core 转换并带提示，
  CLI 不解读协议错误。
- **review 修复**（codex 的 5 条意见，均已用假服务器复现后修复）：
  - 无法解析的行原先只记日志就跳过，LIST / SEARCH / flags FETCH 会把缺失当成"已删除"——
    SEARCH 那一行坏掉就会删光整个文件夹的本地邮件。现在连接层计数，这三种命令报
    `Incomplete`。复核时又发现 EXAMINE 同样漏查：EXISTS 行丢失时按 0 处理，同步"成功"并删掉
    全部缓存邮件；现在 EXAMINE 也拒绝不完整响应，缺 EXISTS / UIDVALIDITY 视为该文件夹失败。
    之后把完整性检查从各调用处收进 `Connection::execute`（默认严格，只有拉邮件头用
    `execute_lossy`），新命令不会再漏掉。
  - 同一 UID 的 FETCH 属性分在多条响应时，原先只取带邮件头的那条，flags 变成默认值；现在按
    UID 合并，缺 FLAGS 或邮件头的邮件不入库（该批不完整，不推进游标）。
  - 有子文件夹的邮箱被删除后变成 `\Noselect`，原先邮件永远留在本地；现在结束这一代 UID。
  - PREAUTH 之后仍发 LOGIN 会被拒绝并误报为密码错误；现在跳过登录。
  - CLI 直接打印邮件标题、发件人、文件夹名和服务器错误文本，可被 ESC 序列操纵终端；现在统一
    转义 C0/C1 控制字符和 bidi 覆盖字符，再计算列宽；`main` 输出的最终错误也经过转义。JSON
    输出本来就会转义。
- **草稿远端副本**：删除文件夹 / UIDVALIDITY 变化时由 repository 同时清空三列，并在表上加
  CHECK，防止 `ON DELETE SET NULL` 留下悬空 UID。

## 方案

- `catmail-proto`：`imap/connection.rs`（驱动、超时、命令/认证流程）、`imap/session.rs`
  （连接、STARTTLS、登录、LIST、EXAMINE、UID SEARCH、按批 UID FETCH、flags 增量）、
  `imap/utf7.rs`、`mime.rs`、`tls.rs`。
- `catmail-store`：`folders.rs`（对账、UIDVALIDITY 重置、游标、计数）、`messages.rs`
  （upsert、flags、删除、列表）；schema 给 `folders` 加 `remote_name`、`selectable`，`drafts`
  加 CHECK 和计数触发器；列表按 Date（缺失时用 INTERNALDATE）、再按 UID 排序，索引与之一致。
- `catmail-core`：`engine.rs`（从 `client.rs` 拆出，含阻塞 DB 助手和同步锁）、`sync.rs`、
  `auth.rs`；`MailClient` 新增 `sync_account`、`list_folders`、`get_folder`、`find_folder`、
  `list_messages`，builder 新增 `tls`。
- `catmail-cli`：`catmail sync [account]`、`catmail ls [folder] [-a] [-n] [--offset]`，
  表格按显示宽度对齐中文，日期用本地时区（jiff）。
- `catmail-testkit`、`testenv/dovecot/`、`testenv/fixtures/`、flake 的 `imap` 开发环境、
  CI 的 Dovecot job。

同步优化：快速路径用文件夹的计数和最大 UID 判断，不再每次加载全部本地 UID；只有新邮件时只 `UID SEARCH UID <游标>:*`，条数对得上就不拉完整 UID 列表；flags 只拉
`1:<本地最大 UID>`；某批拿回的邮件少于请求时不推进游标。

验证：新增测试共 53 个——proto 会话测试 12 个（TLS、STARTTLS 及明文注入、AUTH=PLAIN 与
非 ASCII 密码、错误密码、重复/倒序/主动响应、断线、无 CONDSTORE），同步测试 11 个（初次、
增量、只有新邮件时的窄 SEARCH、无 CONDSTORE、UIDVALIDITY 改变、断点续传 450 封、乱序响应、文件夹删除、错误密码、
并发同步、删除账户取消同步），review 修复 12 个（先确认不修就失败），store 6 个，其余为单元测试和 1 个 Dovecot 集成测试。对两处关键逻辑（UIDVALIDITY 重置、
CONDSTORE 快速路径）做了变异测试，确认测试会失败。对 Dovecot 2.4.5 跑通集成测试（含重复
运行，/simplify 修改后再次跑通），并用 CLI 手动走了一遍 `account add` → `sync` → `ls`。
