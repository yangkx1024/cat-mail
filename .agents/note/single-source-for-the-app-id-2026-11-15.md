# 应用标识只在一处定义

- 日期：2026-11-15
- 类型：重构

## 背景

`net.yangkx.catmail` 分别写在 `catmail-frontend` 的 `APP_ID`、`catmail-core` 拼数据目录的
`ProjectDirs::from("net", "yangkx", "catmail")`（macOS 上就是目录名）、打包文件（`.desktop`、manifest、
appdata、`Info.plist`）和两个打包脚本里，改名漏一处就对不上（#308）。

## 决策

- **完整的 id 放在 core**：macOS 的数据目录按 bundle id 命名，Flatpak 的数据目录按 app id 命名，它不只是桌面的
  概念；core 拼目录时本来就在用它。frontend 重新导出，不再自己定义。
- 拼目录的三段由 `project_dirs()` 从 `APP_ID` 拆出，不再另写一份。起初写了三个常量加 `const fn` 断言在编译时
  核对，但那仍是两份拼写，只为让它们一致；拆分只在取目录时做一次，没有代价。
- 包里的文件引用不了常量，由 frontend 的集成测试读仓库里的文件核对（它不测 frontend 的代码，不放进模块树）；
  每个文件查它该含的片段，不依赖 YAML 的排版。测试不 grep 脚本，脚本改为从 manifest 和 `Info.plist` 读 id。

## 方案

- `catmail-core/src/config.rs`：`APP_ID`，`project_dirs()` 拆它的三段；`catmail-frontend/src/lib.rs` 改成
  `pub use catmail_core::APP_ID`；autostart 的测试也不再写死 id。
- `catmail-frontend/tests/packaging.rs`：`.desktop` 的文件名、`Icon=`、`StartupWMClass=`，manifest 的文件名、
  `id:` 和安装路径，appdata 的 `<id>`、`<launchable>`、`<developer id>`，`Info.plist` 的 `CFBundleIdentifier`。
- `build-flatpak.sh` 取 `packaging/linux/` 里带 `id:` 的那个 manifest（模块的 `.yml` 没有），读出 id；
  `build-dmg.sh` 用 `plutil -extract` 从 bundle 的 `Info.plist` 读 bundle id，CLI 签成 `<bundle id>.cli`。
- 验证：新测试；临时改坏 `.desktop` 的 `StartupWMClass` 测试失败。
