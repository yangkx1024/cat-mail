# nsh 换回 crates.io 的 0.5.0

- 日期：2026-11-12
- 类型：重构

## 背景

nsh 一直用 git 依赖钉在 main 的 `506b539`（为了 #966、#1048、#1051、#1068、#1233、#1241、#1310、#1326、#1359、
#1387 这些修复），`deny.toml` 因此把 longbridge/nsh 放进了 `allow-git`。nsh 发了 0.5.0（tag `v0.5.0`，
`f4d68f7`），`506b539` 是它的祖先（多出 159 个提交），钉住的修复都在里面。

## 决策

- 四个 nsh crate 都换成 `0.5.0`，feature 不变（Linux 的 `webview` 在 0.5.0 里照旧）。catmail-gui 不用改代码：
  macOS 上编译、clippy 都没有新的报错。
- 新版里看过、暂不采用的 API：
  - Rust 的托盘（`set_tray`，#1562）和单实例（#1563）：托盘挂在一个视图的 surface 上，单实例只在 nsh 自己的
    `run_app` 里、按 `nsh.json` 打开，而且只有 Linux 和 Windows。catmail 有自己的外壳，关掉所有窗口后仍要托盘，
    角标是自己画的，单实例三个平台一套（`catmail_frontend::instance`），都比它多。Wayland 上托盘点击的
    activation token（#1615）我们的 ksni 托盘早就转给了窗口。
  - `run_on_main_thread`（#1382）：在本轮提交之后才跑、只能从视图里调；`shell::on_main` 随时可用，就几行，留着。
  - `on_app_quit`（#1558）：退出流程是外壳自己的。
  - `ElementRef::evaluate`（#1432）：Linux 后端还不支持，读信区继续用注入脚本回报。
- 不用改就得到的：选中文字上右键出系统的 Copy / Select All 菜单（#1517，读信区头部、设置、通知里的可选文字），
  macOS 输入框失焦时提交输入法的组字（#1507）、候选窗位置（#1502），窗口不在前台时文件面板在最前（#1488），
  任务结束就释放清理闭包（#1664），视图移出视口后不再逐帧更新（#1600），只在键盘移动焦点时显示
  `focus-visible` 和焦点的 tooltip（#1519、#1653），web view 的麦克风和摄像头默认拒绝（#1559，读信区不用声明）。
- 许可证的例外仍然需要：webpki-roots 现在来自 nsh-core 的 ureq（图片下载）和 tokio-tungstenite，nsh-core 自己的
  WebSocket 改用系统信任库（#1543）；target-lexicon、winx 不变。注释里"钉在未发布的提交上、发版后再看"去掉。

## 方案

- 根 `Cargo.toml` 的 nsh 依赖改成版本号，删去列举 git 修复的长注释；`Cargo.lock` 随之更新。
- `deny.toml`：移除 `allow-git` 里的 longbridge/nsh，更新例外的说明。
- `crates/catmail-gui/.claude/skills/` 换成 `v0.5.0` 的版本（README 记 tag 和 commit）。
- `crates/catmail-gui/ROADMAP.md` 勾掉"换回 crates.io"，`docs/architecture/gui.md` 改依赖的说明。
- 验证（macOS，本机没有 Nix，直接用 cargo）：`cargo fmt --all --check`、
  `cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`（957 个通过）、
  `cargo deny check` 通过。Linux、Windows 的构建由 CI 检查。新版改了 flex 列的尺寸（#1625）和边框的绘制顺序
  （#1546），要在两个平台上看一眼三栏、设置对话框和写信窗口的布局。
