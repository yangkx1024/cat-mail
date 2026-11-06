# 列表的加载协调逻辑移进 catmail-frontend（ListModel）

- 日期：2026-11-06
- 类型：重构

## 背景

#241：列表的零件（`Removal`、`Reveal`、`Days`、`Reselect`）在 frontend 有测试，把它们串起来的加载协调还在 gui：
换来源作废旧结果、读取计数、"过期"标记、服务器搜索结果、按页追加还是整体替换、结果没变不替换、去掉不在了的
勾选、删除或移动后打开下一封、通知点开的邮件出现时打开。这些行为要编译 GTK、WebKit 和 nsh 才能测，实际没有
覆盖。`refresh` 用 `Rows::loading` 加 `stale` 又写了一遍 `Coalesce`；`loading` 放在 `Rows` 里，同步期间每 200 ms
一次的刷新哪怕行没变也让所有读 `Rows` 的地方重算两次；`items` 把每行再复制一份，翻页时整体重建，翻到第 n 页
累计 O(n²) 次复制。

## 决策

- 分成两样，都在 `catmail_frontend::list`（`list/model.rs`）：
  - `Listing`：来源、行、按天分组、条目和还有没有更多。条目只存日期标题和行的下标（`LineAt`），视图要时再取
    （`line`、`keys`、`line_of`）；翻页时只追加这一页的条目。gui 把它放在一个 nsh signal 里，只在读回来的和显示的
    不一样时更新。
  - `ListModel`：换来源（`show`，世代加一、`Coalesce` 重置、等待都清掉）、刷新（`refresh`，读至少一页或已显示的
    行数）、下一页（`more`）、服务器搜索结果（`found_on_server`）、`Removal` 和 `Reveal`。读取由它给出 `Fetch`，
    `Fetch::run` 读；`loaded` 返回 `Loaded`：要换上或追加的行（没变就没有）、选中怎么走（`Select::Keep` /
    `Open` / `Clear`）、还勾选着哪些、要不要再读一次。先后顺序：`removal.read`，行变了才按 `Reselect` 定选中、
    去掉不在了的勾选；移到下一封时忘掉通知要的邮件，否则 `reveal.read`（行没变也看）。
- 和 issue 的建议不同，模型不持有 selected 和 checked：它们仍是 gui 的 signal（读信区、命令、行都在读），由调用方
  传进来，和侧栏模型（#243）一样，避免两份状态。
- `loading` 单独一个 signal（`Loading`），只在读与不读之间切换时写。
- 搜索框的防抖计数和 `CATMAIL_GUI_OPEN` 仍在 gui：一个是输入的节奏，一个是调试用的环境变量。

## 方案

- `catmail-frontend`：`list/model.rs`（`Listing`、`ListModel`、`Fetch`、`Loaded`、`Rows`、`Select`、`Line`、`LineKey`）
  和表驱动测试：读取中再要求刷新会补一次、旧来源的结果丢掉、翻页追加和结果没变不替换、移走选中的邮件后等到它
  离开再打开下一封并去掉勾选、通知的邮件出现时打开（行没变也算）、用户先打开别的就不等、移到下一封优先于通知、
  服务器结果只给对应的那次搜索、翻页时条目接着最后一天往后排。`Coalesce::is_running` 重新加回。
- `catmail-gui`：`mailbox/list/mod.rs` 去掉 `Rows`、`Item`、`Loads`，`show` / `refresh` / `load_more` / `read` / `reveal` /
  `removal_settled` 只调模型、按 `Loaded` 设 signal；`rows.rs` 的格子读 `Listing::line`。
- gui.md 的邮件列表一节和节流的说法。
