# Code review 界面采用 GitHub 式单栏 diff + 行内评论

code review 界面（`/code-review`）采用 GitHub 风格：单栏 unified diff
（CodeMirror `unifiedMergeView`），评论内联展示在代码行下方。备选方案：
B. 双栏 side-by-side diff + 评论侧栏列表；C. 文件树导航 + gutter 标记弹层。
用户评审三变体方案后直接选定 A。

**Consequences**：评论以（文件, side, 行范围）锚定——side 为 new（新文件
行）或 old（删除块内的行，经 `posAtDOM` 定位 chunk）。拖动选择后点浮动
popup 图标即可评论，选择按行粒度归一化（不到字符级），便于 agent 定位
问题区间；gutter 的「+」按钮等价于锚定单行。视觉语言：增删 = merge 的
红绿色条，评论 = 左侧 3px 蓝色连续色条（含 widget 行，跨删除区不断）；
内容区高亮只属于草稿态，评论提交后代码区恢复干净，信号仅留在色条与
卡片。蓝色 accent 由 `--review-accent` 变量统一控制。评论**没有回复
功能**：这是专注于 agent 的工具，评论只给 agent 看，不考虑团队协作。
评论存于 pinia store（内存态）；把评论导出给 agent 的功能刻意推迟，
届时只需读 `useReviewCommentsStore`。diff 数据目前是 stub，等 daemon
提供 diff API。
