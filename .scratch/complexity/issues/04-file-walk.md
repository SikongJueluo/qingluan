# 04 · 文件扫描、排除与跳过统计

Status: resolved
Blocked by: 03

- `ignore` crate 遍历（自带并行），尊重 `.gitignore`/`.ignore`，补 jj 的
  ignore 文件名（`add_custom_ignore_filename`）。
- 默认额外排除：`target/`、`node_modules/`、`vendor/`、`third_party/`、
  `.git/`、常见生成物（`*.min.js`、`*.min.css`、`*_pb2.py`、`*.pb.go`、
  lockfile 等）。
- 单文件超过体积上限（默认 1 MiB）跳过并计入 `tooLarge`。
- 无 grammar 的扩展名计入 `unsupported`（不静默丢）。
- 跳过统计必须能被 CLI 打印：`{generated, unsupported, tooLarge}`。
- 配置面（`qingluan.toml`，走 `qingluan-config` 四层合并）：
  `[complexity] top / cc_threshold / cognitive_threshold / exclude / include`。
- 单测：临时目录里放 `target/`、一个 `.min.js`、一个不支持后缀、一个大文件，
  断言各自计入正确的跳过桶且不出现在结果里。

## Comments

- 2026-09-30 agent：完成。`src/scan.rs`：`ignore` 遍历（显式 `require_git(false)`，
  否则非 colocate 的 jj 仓库不吃 `.gitignore`）、内建排除目录、生成物 glob、
  1 MiB 体积上限、`include`/`exclude` glob、跳过三桶统计；文件解析走 `rayon` 并行，
  结果按路径排序（确定性）。
- 配置 `[complexity] top / cc_threshold / cognitive_threshold / exclude / include`
  已进 `qingluan-config`（四层合并，含单测）。
- 未加 `add_custom_ignore_filename`：jj 只在 colocate 时提供 `.git`，仓库里并没有
  `.jjignore` 这种额外忽略文件名，编造一个会误导。实测 `require_git(false)` 已让
  `.gitignore` 生效。
- 退出依据：`tests/scan.rs` 4 条 —— target/node_modules 直接排除且不计入任何桶、
  `.gitignore` 生效、generated/unsupported/tooLarge 各进对应桶、缺失路径报
  `NotFound`、`analyze_path` 对无 grammar 文件返回 `None`。
- 实测本仓：164 文件 / 1819 函数 / skipped 120（全部是 `.vue` 111 + `.sql` 4 +
  `.proto` 4 + `.sh` 3 之类的「有代码但没 grammar」）。
