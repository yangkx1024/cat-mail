# 用 deslop-please 在 CI 里守住模块结构

- 日期：2026-11-25
- 类型：功能

## 背景

deslop-please 按《A Philosophy of Software Design》检查模块循环、tangle、字段外泄、浅模块、过宽目录和过长文件。工作区现有 75 个问题，其中 `Engine` 字段外泄、store 表之间互相调用等需要先设计再改，不能一次清完；但新代码不应再增加新的问题。

## 决策

- 用 `.deslop-please.allow` 记下现有问题，CI 跑 `deslop-please --allow .deslop-please.allow`：列表里的不报，新出现的失败。列表中的循环和 tangle 变小不算新问题，修掉的条目也不会让 CI 失败。
- CI 在 macOS 上跑：工具只索引当前配置下的 `cfg` 代码，列表是在 macOS 上生成的，换到 Linux 会看到 GTK 那一侧的模块，与列表对不上。仓库公开，macOS runner 不额外收费。
- 固定 0.1.4 版本：规则和 key 的格式随版本变化，升级时一起重新生成列表。
- AGENTS.md 写明：新问题要从设计上修，不要靠挪代码让它消失，也不要加进列表。

## 方案

- `.deslop-please.allow`：`deslop-please --update-allow` 生成的 75 条。
- `.github/workflows/ci.yml`：新增 `structure` job（macOS，`rust-analyzer` 组件，`taiki-e/install-action` 安装 `deslop-please@0.1.4`）。
- `AGENTS.md`：在 Build and test 中说明如何运行和处理新问题。

验证：本地 `deslop-please --allow .deslop-please.allow` 输出 ok、退出码 0；CI 结果见 PR。
