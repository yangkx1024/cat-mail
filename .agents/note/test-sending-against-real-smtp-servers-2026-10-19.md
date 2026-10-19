# 对真实 SMTP 服务器测试发信

- 日期：2026-10-19
- 类型：功能

## 背景

M3 的发信只对 `catmail-testkit` 的假 SMTP 服务器测过（PR #8 的 Known limitations 第一条）。
假服务器是我们自己按自己的理解写的，EHLO 多行答复、STARTTLS 之后重新 EHLO、AUTH、SIZE、
点填充、RCPT 被拒后的 RSET 是否和真实服务器对得上，只有对真实实现跑一遍才知道。

## 决策

- **Dovecot 的 submission 服务 + mailpit**，而不是只用 mailpit：
  - submission 是生产环境里真实使用的提交服务器，AUTH 走和 IMAP 相同的 passdb，STARTTLS /
    隐式 TLS、SIZE（`submission_max_mail_size`）都是真实实现；
  - 它必须转发给一个 MTA，mailpit 正好是收信端，还带 HTTP API，可以读回信封和原文，核对
    "发出去的就是存进 Sent 的那份"；
  - mailpit 的 `--smtp-allowed-recipients` 让真实服务器拒绝某个收件人（Dovecot 把它对 RCPT
    的拒绝转回来），不用伪造。
  - 没选 Postfix：要 root 或大量配置才能以普通用户跑起来。
- **复用现有的 Dovecot**（`testenv/dovecot/run.sh` 同时起 mailpit），一个 CI job 跑 IMAP 和
  SMTP 两组测试。开发环境从 `.#imap` 改名为 `.#testenv`，里面有 dovecot、mailpit、openssl。
- **证书**：`run.sh` 用 openssl 为 127.0.0.1 生成自签名证书（`CA:FALSE`，rustls 不接受 CA
  证书作服务器证书），测试用 `TlsConfig::with_roots_only` 只信任它。IMAP 仍用明文登录，
  原有测试不变。
- **端口**从 IMAP 端口推出（默认 10143 → 提交 10587 / 10465，mailpit 11025 / 11080）。
- **mailpit 的 HTTP API 用手写的 HTTP/1.0 GET 访问**：只需要两个 GET，HTTP/1.0 的响应不会
  分块，不值得为测试引入 HTTP 客户端；JSON 用已有的 `serde_json`（core 的 dev-dependency）。
- **mailpit 存信时会给只在信封里的收件人加一行 `Bcc:`**。所以"邮件里没有 Bcc"看的是 Dovecot
  Sent 里的副本，再断言 mailpit 收到的原文以这份副本结尾（服务器只在前面加 trace 头）。
- 测试可以对同一组服务器重复运行：邮件按 Message-ID（mailpit 的搜索 API）或带时间戳的
  主题区分。
- **发信测试用单独的 Dovecot 用户 `sender`**，`dovecot.rs` 用 `catmail`：后者会把 Sent 的
  `tmp/` 设成只读、数 Sent 里的副本，共用一个账户时两组测试不能同时跑（见下面验证里的那次
  失败）。分开后可以并发。
- **两组测试共用 `tests/testenv/`**：读 `run.sh` 打印的变量、跳过或失败（一个开关
  `CATMAIL_TESTENV_REQUIRED`，取代原来的 `CATMAIL_TEST_IMAP_REQUIRED`：同一个脚本、同一个
  CI job，分两个开关没有意义）、建客户端和账户、在 Sent 里找邮件、本次运行的标签。

## 方案

- `testenv/dovecot/dovecot.conf.in`：`protocols = imap submission`、SSL 证书、转发到 mailpit、
  1 MiB 上限、submission-login 的两个端口。`run.sh`：生成证书、先起 mailpit 并等 API 就绪、
  导出 `CATMAIL_TEST_SUBMISSION(S)`、`CATMAIL_TEST_CERT`、`CATMAIL_TEST_MAILPIT`，任一进程
  退出就结束并清理。
- `flake.nix`：`testenv` 开发环境。`.github/workflows/ci.yml`：job 改名，跑
  `--test dovecot --test submission`。
- `crates/catmail-core/tests/submission.rs`：STARTTLS 发信（中文主题与收件人、以点开头的
  行、密送、附件）并保存到 Sent，核对信封与逐字节一致；隐式 TLS；真实服务器拒绝一个收件人
  时整封不投递；超过 SIZE 不发送；密码错误。
- 文档：README、`docs/architecture.md`（测试环境）、AGENTS.md。
- 验证：本地从冷启动按 CI 的步骤跑了 18 次（每次重新起服务器、两组测试、停止后检查没有
  残留进程）。只有 1 次失败（Dovecot 测试第二次同步时超时），那次与上一次被中断的模拟脚本
  并发，两组测试同时改同一个 Dovecot 账户；于是给发信测试换了单独的用户，之后两组测试并发
  跑 3 轮也都通过。fmt、clippy、全部测试、`nix fmt` 通过。
- /simplify（四个方向）之后的清理：上面的单独用户和共用模块；mailpit 按 Message-ID 搜索，
  不再列出 1000 封再过滤（服务器上信多了以后，"没有投递"的断言会失去意义）；草稿辅助函数
  `note`，只有按主题查找的那个测试需要运行标签；`ServerConfig` 在读变量时就建好、保留变量里
  的主机。没采纳：把 `run.sh` 的端口改成单独可设的变量（测试只读导出的变量，推算出来的默认
  值不是约定）。
