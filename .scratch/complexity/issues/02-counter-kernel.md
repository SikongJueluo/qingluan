# 02 · 语言无关计数器内核

Status: resolved
Blocked by: 01

内核只吃「函数子树 + 该语言的决策节点分类函数」，自己负责遍历、嵌套层级、
逻辑序列、nloc/params 统计。语言模块不碰算术。

- 输出 `{cc, cognitive, nloc, params, maxNesting}`，不发明复合分数。
- CC 口径（CC1）：`1 +` if/else-if/loop/ternary/每个 `case`（default 不计）/
  catch/短路布尔运算符个数/`match_arm`（通配臂不计）。
- 认知复杂度按白皮书 Appendix B：
  - `if`/ternary/switch/loop/catch：`+1 + 当前嵌套层级`，且抬高块内层级
  - `else if`：`+1`（不罚），抬高块内层级
  - `else`：`+1`（不罚），抬高块内层级
  - `switch` 全部 case 合计只算 1 次（结构性），default 不计
  - 无标签 break/continue、提前 return、一元 `!`：0 分
  - 带标签 break/continue、goto：`+1`（不罚、不抬层级）
  - 嵌套函数/闭包：自身 0 分，抬高其内部层级
  - 逻辑运算符序列：每段「新的同类序列」+1（算法以 SonarJS/sonar-java
    源码实测为准，见 `.scratch/complexity/research/logical-sequences.md`）
- golden test（调研文档 §5.4）：
  - CC=7 的 `classify` 示例 → cc 7
  - 认知=15 的 `classify(user, items)` 示例 → cognitive 15
  - 逻辑序列：`a && b && c`=1、`a && !(b && c)`=2
  - ESLint classic/modified 官方示例（3 case + 1 if → classic 5 / modified 3）
- 语法错误文件：结果降级但不 panic（断言只要求「不 panic + cc >= 1」）。

## Comments

- 2026-09-30 agent：完成。全部算术在 `src/kernel.rs`；遍历用显式工作栈，深树不会爆栈。
- 逻辑序列落地为 sonar-java 的「flatten 整棵 component（只穿过操作数与括号）
  再比相邻 operator」算法，实测与白皮书全部公开例子一致：`a&&b&&c`=1、
  `a||b&&c||d`=3、`a&&!(b&&c)`=2、`a||b||c||d`=1。JS 的 `||`/`??` 免计偏差不跟进；
  `??` 按普通短路运算符计（白皮书未提，已在 `LogicalOp::Nullish` 注释里写明）。
- 与 sonar-java 的两点差异（均以实现为准并写进代码注释）：
  1. `else` 只 +1，块内层级由外层 `if` 一次性抬升（这是让 else/else-if 落在正确
     深度、又不需要 visitor 那套 nesting 补偿的等价写法）；
  2. `if` 条件里的三元会比 sonar-java 多算一层嵌套（我们整棵 `if` 子树一起降层）。
- 退出依据：`tests/golden.rs` 13 条全绿，含 ESLint classic CC=7、Sonar 例
  cognitive=15/cc=8/maxNesting=4、switch 3 case+default → cc 4 / cognitive 1、
  Rust 边界与限定名、Python for-else 不计、语法错误恢复。
