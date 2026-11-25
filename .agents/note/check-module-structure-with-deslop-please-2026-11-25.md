# 用 deslop-please 在 CI 里守住模块结构

- 日期：2026-11-25
- 类型：功能

## 背景

deslop-please 按《A Philosophy of Software Design》检查模块循环、tangle、字段外泄、浅模块、过宽目录和过长文件。工作区现有 75 个问题，其中 `Engine` 字段外泄、store 表之间互相调用等需要先设计再改，不能一次清完；但新代码不应再增加新的问题。

## 决策

- 用 `.deslop-please.allow` 记下现有问题，CI 跑 `deslop-please --allow .deslop-please.allow`：列表里的不报，新出现的失败。列表中的循环和 tangle 变小不算新问题，修掉的条目也不会让 CI 失败。
- 三个平台都要检查：工具一次只索引一种配置，其他 OS 的 `cfg` 代码不在其中。在 macOS 上用 `--target` 交叉索引，Linux 的结果与 macOS 相同；Windows 上 GUI 是空壳，原有的 15 条不再出现，又多出 2 条 frontend 的字段外泄（`Clock::hours`、`Image::bytes`）。只检查一个平台会漏掉这类问题。
- 只用一台 Linux runner，以 `--target` 依次检查三个平台，不再每个平台各开一个 runner：列表本来就是用 `--target` 交叉生成的，CI 用同样的方法检查，两边一定对得上；而且只需一台 runner、一份缓存。曾经试过三平台各开一个 runner，Linux 本机（装了 GTK 库）的结果与交叉索引完全一致，说明交叉索引可靠，也不需要装 GTK 库。
- 三个平台共用一份列表，取各平台结果的并集。`--update-allow` 只会写当前平台的结果，所以 `scripts/deslop.sh --update` 分别检查三个平台再合并；`scripts/deslop.sh` 不带参数时就是 CI 跑的检查。
- 固定 0.1.4 版本：规则和 key 的格式随版本变化，升级时一起重新生成列表。
- AGENTS.md 写明：新问题要从设计上修，不要靠挪代码让它消失，也不要加进列表。

## 方案

- `.deslop-please.allow`：`scripts/deslop.sh --update` 生成的 77 条（macOS / Linux 的 75 条，加上 Windows 才有的 2 条）。
- `.github/workflows/ci.yml`：新增 `structure` job（ubuntu，运行 `scripts/deslop.sh`；`rust-analyzer` 组件，`taiki-e/install-action` 安装 `deslop-please@0.1.4`）。
- `AGENTS.md`：在 Build and test 中说明如何运行和处理新问题。

- `scripts/deslop.sh`：检查三个平台；`--update` 重新生成列表。

验证：本地对 `aarch64-apple-darwin`、`x86_64-unknown-linux-gnu`、`x86_64-pc-windows-msvc` 三个 target 分别运行 `deslop-please --allow .deslop-please.allow`，都输出 ok；CI 结果见 PR。
