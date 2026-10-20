# 导入终端配色：主题的纯逻辑部分（M5）

- 日期：2026-10-20
- 类型：功能

## 背景

M5 的图形界面要能换主题，用户希望直接导入常用终端和命令行工具的配色。终端配色只给背景、前景和
16 个 ANSI 色（有时加光标、选区），而 gpui-kit 的主题有上百个颜色键。这一步只做与 GPUI 无关的
部分：读配色文件、把调色板推导成 gpui-kit 主题、内置主题；接入窗口（主题注册、设置页、导入对话框）
在 M5 的 GUI 部分做。

## 决策

- **所有主题都从调色板推导。** 内置主题也写成终端调色板，与导入的走同一套规则，不维护两套手写的
  JSON。gpui-kit 自己的主题 JSON 原样接受（没有调色板）。
- **推导规则照设计稿"终端配色 → 界面角色"那张表**：表面、边框是背景向前景混合一定比例（sRGB，同
  CSS `color-mix(in srgb)`）；强调色取蓝，危险 / 成功 / 警告 / 信息取红 / 绿 / 黄 / 青，普通与亮色
  中第一个对背景达到 3:1 的（暗色先试亮色）；次要文字提到 4.5:1（不超过前景本身的对比度）。设置
  gpui-kit `ThemeConfigColors` 的每一个键，不让任何颜色落回 gpui-kit 的灰色默认值。
- **与设计稿 JS 的不同**：
  - gpui-kit 把选中行（list / table active）的不透明度压到 20%、文本选区压到 30%，不管主题写什么。
    所以选中行写成"强调色 + 透明度"（亮 11%、暗 20%），画出来等于设计稿的不透明混色；配色文件自带
    的不透明选区色换算成在 30% 下合成出同一颜色的半透明色。
  - Solarized 的"亮色"槽是灰色，Solarized Dark 按"暗色先试亮色"会把强调色取成灰。挑色时先跳过
    灰色（三通道最大差 < 32），色相都读不清时才退回。设计稿里的主题都不受影响。
  - 填充色按钮的悬停 / 按下：向背景 / 前景各混 12%，所有色相一致；淡色按钮（`button.danger` 等）
    是背景上的淡色加同色文字（文字提到 4.5:1）。
- **按内容识别格式，扩展名只用来打破平局**：Ghostty 的配色文件没有扩展名，配色文件也常被随意改名。
  支持 Alacritty、kitty、Ghostty、WezTerm、Windows Terminal（单个配色或带注释、尾逗号的
  settings.json，可含多个配色）、iTerm2、foot（`[colors]` / `[colors-dark]`）、Xresources（含
  `#define`）、base16 / base24（平铺或 `palette:` 下）、gpui-kit 主题 JSON。
- **不加新的解析库**：TOML 用锁文件里已有的 `toml` 1.1.8，iTerm2 的 plist 用已有的 `quick-xml`
  0.41（GPUI 的平台库在用）；YAML（base16 只是平铺的 `key: "value"`）和 JSON 注释手写处理。
  锁文件只多了 catmail-gui 的四条依赖引用。
- 只有普通色或只有亮色时互相补齐；缺背景 / 前景或某个色相两种都缺时报错，错误信息是给用户看的
  英文句子（"It doesn't set color 4 (blue)."）。名字取文件里的（kitty 的 `## name:`、WezTerm 的
  `[metadata]`、base16 的 `scheme` / `name`、Windows Terminal 的 `name`），否则由文件名生成。

## 方案

- `crates/catmail-gui/src/theme/`：`palette.rs`（`Rgb` 的解析、WCAG 对比度、混色；
  `TerminalPalette`）、`import.rs`（`import(file_name, contents)`、`ThemeFormat`、`ImportedTheme`、
  `ImportError`）、`derive.rs`（`theme_config`、`theme_set_json`，后者写出去掉 null 的 `ThemeSet`
  JSON，供主题目录使用）、`builtin.rs`（11 个内置主题，默认 cat-mail Light / Dark）。
  `mod.rs` 暂时 `allow(dead_code, unused_imports)`，接入界面时去掉。
- 依赖：根 `Cargo.toml` 加 `quick-xml`、`toml`；catmail-gui 引用它们和 `serde_json`、`thiserror`。

验证：`cargo fmt`、`clippy -D warnings`、全部测试通过。新增 25 个单测：每种格式一个样例（含带注释的
Windows Terminal settings.json、带 `#define` 的 Xresources、嵌套与平铺的 base16 / base24、iTerm2
plist）、错误情况（空文件、无关文本、缺背景、坏颜色、缺色相、坏 JSON、空主题集）、所有内置主题的
对比度保证、亮暗判断、每个颜色键都已设置且没有多余的键、经 gpui-kit 的 `ThemeSet` 往返不变、不透明
选区色在 30% 上限下仍得到原色、Solarized Dark 取色相而不是灰色。
