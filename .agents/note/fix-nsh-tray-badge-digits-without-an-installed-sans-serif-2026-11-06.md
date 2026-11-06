# 修复：sans-serif 指向没装的字体时，nsh 托盘角标没有数字

- 日期：2026-11-06
- 类型：修复

## 背景

nsh-spike 第一次跑 CI，Linux 上 `the_icon_is_drawn_at_every_size_with_its_count` 失败：角标里只有 6 个白色像素，
数字没画出来。NixOS 和 macOS 上一直通过。运行时走的是同一段代码，这类 Linux 系统上托盘角标就没有数字。

复现条件：fontdb 的 `load_system_fonts` 按 fontconfig 的别名定 `sans-serif`（`system-fonts` 打开了 `fontconfig`），
但它对每条别名都取 prefer 列表的第一个，不看装没装，最后一条胜出。Ubuntu 的 `/etc/fonts/conf.d` 有多条
sans-serif 别名，最后落到一个没装的字体上；角标的 `<text font-family="sans-serif">` 就找不到字体，文字被跳过。
原来的 `tray.rs` 就是这样，不是拆分 `tray/` 时引入的。

## 决策

- 加载系统字体后检查 `sans-serif` 能否查到；查不到就依次试几种常见的无衬线字体（DejaVu Sans、Noto Sans、
  Liberation Sans、Cantarell、Ubuntu、Arial、Helvetica），都没有就用库里第一个字体，并记一条 info 日志。
- 没有改成在 SVG 里写具体字体名：角标 SVG 是 frontend 的、gpui 版也用，字体有没有装是画的那一方的事。
- 没有自己调 fontconfig（`fc-match`）：要加原生依赖，只为了一个数字不值得。
- gpui 版的角标由 GPUI 的 `SvgRenderer` 画，它的字体库不归这里管，没有动。

## 方案

- `crates/catmail-nsh/src/tray/mod.rs`：`FONTS` 加载后调 `installed_sans_serif`。
- 测试：`sans_serif_is_a_font_that_is_installed` 把 `sans-serif` 设成不存在的字体，断言修正后能查到（去掉修正时
  失败，`sans-serif is No Such Font`）。Linux 上画角标的测试失败时也写出 `sans-serif` 指向的字体，便于诊断。
- 验证：macOS 上 clippy、tray 测试通过；Linux 靠重跑 CI。
