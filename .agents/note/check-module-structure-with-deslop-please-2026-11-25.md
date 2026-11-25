# 用 deslop-please 在 CI 里守住模块结构

- 日期：2026-11-25
- 类型：功能

## 背景

deslop-please 按《A Philosophy of Software Design》检查模块循环、tangle、字段外泄、浅模块、过宽目录和过长文件。工作区现有 75 个问题，其中 `Engine` 字段外泄、store 表之间互相调用等需要先设计再改，不能一次清完；但新代码不应再增加新的问题。

## 决策

- 用 `.deslop-please.allow` 记下现有问题，CI 跑 `deslop-please --allow .deslop-please.allow`：列表里的不报，新出现的失败。列表中的循环和 tangle 变小不算新问题，修掉的条目也不会让 CI 失败。
- CI 在 Linux、macOS、Windows 三个平台都跑：工具一次只索引一种配置，其他 OS 的 `cfg` 代码不在其中。用 `--target` 在 macOS 上交叉索引，Linux 的结果与 macOS 相同；Windows 上 GUI 是空壳，原有的 15 条不再出现，又多出 2 条 frontend 的字段外泄（`Clock::hours`、`Image::bytes`）。只在一个平台上跑会漏掉这类问题。仓库公开，各平台 runner 都不额外收费。
- 三个平台共用一份列表，取各平台结果的并集。`--update-allow` 只会写当前平台的结果，所以用 `scripts/update-deslop-allow.sh` 在任意一台机器上用 `--target` 分别跑三个平台，再合并成一份。
- 固定 0.1.4 版本：规则和 key 的格式随版本变化，升级时一起重新生成列表。
- AGENTS.md 写明：新问题要从设计上修，不要靠挪代码让它消失，也不要加进列表。

## 方案

- `.deslop-please.allow`：`scripts/update-deslop-allow.sh` 生成的 77 条（macOS / Linux 的 75 条，加上 Windows 才有的 2 条）。
- `.github/workflows/ci.yml`：新增 `structure` job（三平台矩阵，Linux 上装 GTK 和 WebKitGTK 开发库，供索引时运行 build script；`rust-analyzer` 组件，`taiki-e/install-action` 安装 `deslop-please@0.1.4`）。
- `AGENTS.md`：在 Build and test 中说明如何运行和处理新问题。

- `scripts/update-deslop-allow.sh`：重新生成列表。

验证：本地对 `aarch64-apple-darwin`、`x86_64-unknown-linux-gnu`、`x86_64-pc-windows-msvc` 三个 target 分别运行 `deslop-please --allow .deslop-please.allow`，都输出 ok；CI 三个平台的结果见 PR。
