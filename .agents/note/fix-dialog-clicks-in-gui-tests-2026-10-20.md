# GUI 测试关闭对话框动画，修复 CI 上点不到对话框按钮

- 日期：2026-10-20
- 类型：修复

## 背景

M5 的 PR 在 CI 上偶发失败：macOS 上 `settings_change_the_theme`（点设置里的 Dark 后外观仍是 Light），
另一次 Windows 上它和 `deletes_for_good_without_a_trash_folder`（确认框的 OK 点了没反应，超时）同时
失败。本地 Linux 稳定通过。在单独分支上加诊断跑 CI：按钮可见、位置正确，但点击后设置立即读出来仍未改变，
说明点击没有送到按钮。

## 决策

gpui-kit 的 Dialog / AlertDialog 默认滑入（`DialogEntrance::SlideDown`），绘制和命中测试都跟着动画
偏移；动画按真实时间推进。测试的 `click` 按上一帧的位置发出按下和抬起，慢机器上两帧之间对话框已经移动，
点击落空。gpui-kit 只在 `reduce_motion` 时跳过动画，而测试调度器下它不读系统设置，需要测试自己设。

考虑过在产品代码里给对话框设 `DialogEntrance::None`，但这只是测试环境的时序问题，界面保留动画。

## 方案

`mailbox/tests/mod.rs` 的 `open_mailbox` 里 `cx.set_reduce_motion(true)`（截图测试共用它，截到的也是
最终状态）。`docs/architecture/testing.md` 说明原因。验证：本地全部 GUI 测试通过；CI 三个平台见 PR。
