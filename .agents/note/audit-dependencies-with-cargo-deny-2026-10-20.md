# CI 加入 cargo deny 依赖审计

- 日期：2026-10-20
- 类型：功能

## 背景

issue #26：仓库没有任何依赖审计，CI 也不检查。程序自带 TLS 栈、解析不可信的 IMAP / SMTP /
MIME / HTML 输入，依赖里新披露的漏洞、许可证问题或者混进第二套加密实现都没人发现。

## 决策

- 只用 cargo-deny，一份 `deny.toml` 同时管安全公告、许可证、禁用 crate 和来源。issue 里提到的
  `rustsec/audit-check` 没用：它读 cargo-audit 自己的忽略列表，会和 `deny.toml` 两处维护，而且它
  是开 issue，不是写 Security 页。改为 `cargo deny --format sarif` 输出后用
  `github/codeql-action/upload-sarif` 上传到 code scanning，忽略规则只有一份。
- 安全公告在 PR 上也阻塞：新公告要么升级依赖，要么在 `deny.toml` 里带原因忽略，不放宽检查。
  每周一定时跑一次，新公告不用等下一次提交。SARIF 只在 main / 定时任务上传（fork 的 PR 没有
  上传权限，跟踪的也应该是 main 的告警）。
- `bans` 不禁止重复版本：GPUI 带来大量无法统一的重复依赖，警告只是噪音。真正要禁止的是第二套
  TLS / 加密实现：aws-lc-rs、OpenSSL、native-tls、boring（rustls 只用 ring，见 `Cargo.toml`）。
- 许可证白名单按当前依赖树列出的宽松许可证，没出现的不预先放行（cargo-deny 会对没用到的条目
  报警）。`graph.targets` 限定为 Linux / Windows / macOS 的 x86_64 和 aarch64，Android 等目标
  才用到的 crate 不算。
- 现有 7 条公告全是 unmaintained，没有漏洞，都来自 gpui-kit（instant、paste、ttf-parser、
  rustybuzz）和 zbus 5（zvariant、zbus_names、zvariant_derive 已并入 zbus 6，gpui-kit 和 keyring
  都还在用 zbus 5），逐条带原因忽略。

## 方案

- `deny.toml`：advisories / licenses / bans / sources 配置。
- `.github/workflows/audit.yml`：PR、main、每周定时和手动触发；装 cargo-deny 预编译版，先查
  bans licenses sources，再查 advisories，非 PR 时导出 SARIF 上传。
- `flake.nix` 开发环境加 `cargo-deny`；`AGENTS.md`、`README.md` 补上命令和 CI 说明。
- 验证：本地 `nix develop -c cargo deny --locked check` 四项全过、无警告；SARIF 输出格式本地检查过。
