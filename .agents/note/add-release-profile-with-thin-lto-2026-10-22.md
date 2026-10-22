# 为 release 构建加上 thin LTO

- 日期：2026-10-22
- 类型：功能

## 背景

工作区没有 `[profile.release]`，release 构建用的是 Cargo 默认值（opt-level 3、16 个 codegen unit、无 LTO），
`catmail-gui` 约 103 MB。参考 Zed 的 PR zed-industries/zed#65168：它把自家 crate 设为
`opt-level = "z"`、依赖和热点 crate 保持 3，再加上原有的 thin LTO + `codegen-units = 1`，二进制缩小 7%–33%。

## 决策

在 Linux x86_64 上实测了四种配置（`cargo build --release --bin catmail --bin catmail-gui`）：

| 配置 | `catmail` | `catmail-gui` |
|---|---:|---:|
| 默认 | 26,745,392 | 102,855,312 |
| thin LTO + `codegen-units = 1` | 21,481,000（−20%） | 83,847,480（−18%） |
| 再加自家 crate `z`（gui、render 保持 3，依赖保持 3） | 19,901,200（−26%） | 84,474,200（−18%） |
| 全部 `z`（含依赖） | 20,463,032 | 83,255,728（−19%） |

只采用 thin LTO + `codegen-units = 1`。放弃 `opt-level = "z"`：Zed 有上百个自家 crate，而我们的代码只占二进制的一小部分，
体积主要来自 GPUI、Blitz、Stylo 等依赖；按 Zed 的方式只改自家 crate，GUI 反而略大，
连依赖一起改成 `z` 也只再省 0.7%，却要牺牲渲染和布局的速度。CLI 能多省 7%，但不值得为它单独维护一套按包的配置。

Zed PR 里 macOS 的 `-unexported_symbol,__R*` 链接参数也没有加：目前只发布 Linux，也没有打包脚本。

## 方案

根 `Cargo.toml` 新增 `[profile.release]`：`lto = "thin"`、`codegen-units = 1`。
验证：上表的四次 release 构建；fmt、clippy、全量测试照常通过（测试用的是 dev profile，不受影响）。
