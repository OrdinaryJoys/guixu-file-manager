# 归序交叉验证矩阵（2026-07-30）

本文把“代码已实现”“自动测试通过”和“用户在真实窗口中可用”分开记录。只有三者都有证据的能力才标为完成；研究方向或规划项不再写成当前能力。

## 验证分层

| 层级 | 验证内容 | 当前门禁 |
| --- | --- | --- |
| L1 静态契约 | DOM id、设置页目标、Tauri IPC 注册、CSS 变量与结构、Web/App 唯一入口 | `node tools/verify_frontend_contract.mjs` |
| L2 算法与数据 | 搜索、身份、迁移、任务状态机、事务、重复与相似候选、EXIF 方向、旧设置兼容 | `cargo test --workspace`（89 项） |
| L3 编译质量 | 格式、全部 target/feature 的 warning、JS 语法、diff 空白 | fmt、严格 Clippy、`node --check`、`git diff --check` |
| L4 Web 交互 | 搜索、视图切换、设置、计划、执行、历史、撤销、控制台 | 同源 Web 演示真实点击 |
| L5 App 交互 | Tauri IPC、真实索引、系统选择器、窗口缩放、macOS 字体与 WKWebView 渲染 | debug `.app` 真实窗口点击与截图 + 5 场景嵌入式 WebDriver E2E |

## 功能、算法和界面矩阵

| 模块 | 主流实现基线 | 当前实现证据 | 自动验证 | Web / App 验证 | 状态与缺口 |
| --- | --- | --- | --- | --- | --- |
| 前端交付 | Tauri 单一前端、能力最小化 | Tauri 与浏览器启动器都指向 `apps/desktop/web`；旧 Python 原型归档至 `legacy/python-prototype` | L1 | Web 与 App 同一 HTML/CSS/JS | 已统一；禁止再维护第二份页面 |
| 文件夹授权 | 系统目录选择器，不以手输路径为主流程 | Tauri dialog plugin + library id 边界 | L1/L3 | App 系统选择器 | 已完成；后续补安全作用域书签 |
| 索引与对账 | 增量扫描、稳定身份、文件事件合并、失败保守 | worker 扫描、FSEvents 脏目录、generation、incomplete scan 保护 | L2 | App 真实资料库 | 核心完成；真实外置卷组合仍需扩大 |
| 搜索 | SQLite FTS5 + 结构化过滤 + 稳定分页 | Unicode/trigram、短词回退、`ext:`/`size:`、游标 | L2 + L1 | Web demo 与 App 搜索 | 完成；单字大库性能基准待做 |
| 智能文件夹 | 保存查询而非复制文件 | SQLite 持久化查询、执行和删除 | L1/L2 | Web/App 点击 | 完成 |
| 安全操作 | 计划 → 预检 → 意图日志 → 发布 → 撤销 | rename/move/copy/trash、排他发布、BLAKE3、逐项回滚验证 | L2/L3 | App 自动执行真实重命名、历史与撤销 | 核心完成；跨卷和异常断电需持续真机覆盖 |
| 后台任务 | 可恢复状态机、租约、幂等消费、控制事件 | queued/running/pause/cancel/completed/failed + job result 同事务 | L2 | App E2E 回填重复/相似报告 | 核心完成；已补中途冲突与目录符号链接逃逸注入，断电/拔盘矩阵仍待扩大 |
| 精确重复 | 大小 → 快速指纹 → 完整哈希；缓存与确定性保留解释 | BLAKE3 分层、持久缓存、只读报告、受控清理计划 | L2 | Web/App 报告 | 完成；10 万/100 万文件性能门禁待建 |
| 相似文本 | 轻量指纹召回 + 独立证据复核 | 64 位 SimHash + 三段 MIH + 32 分量 MinHash；完整召回与阈值评估单测 | L2 + Criterion | Web/App 同一只读报告与证据字段 | 双阶段完成；真实标注集和可选 embedding 待做 |
| 相似图片 | EXIF 归一化、感知哈希候选、阈值可解释 | 一次限额解码生成 dHash/pHash + 三段 MIH；EXIF 与压缩/缩放单测 | L2 + Criterion | Web/App 同一只读报告与证据字段 | 双阶段完成；真实图片集阈值和裁剪/旋转增强待做 |
| UI 设计系统 | 语义令牌、一致组件、响应式密度、可访问焦点 | 统一字体栈、色彩/圆角/间距令牌、900×620 断点 | L1/L3 | Web 与 App 双尺寸截图 | 进行中；仍有少量历史硬编码色，按组件迁移 |
| 动效 | 短时长、状态解释、减少动态效果 | 微交互、弹窗、通知、骨架、任务进度、渐进式 View Transitions | L1/L3 | Web 点击；App WKWebView 复验 | 已建立；不支持 View Transition 时同步回退 |
| 设置 | macOS 可发现入口、分组与即时反馈 | 通用/外观/扫描性能/本地数据四分区，SQLite 持久化 | L1/L2 + 原生 E2E | Web/App 点击与 WebDriver 保存后重读 | 功能完成；独立原生 Settings 窗口与应用菜单入口待做 |
| 平台增强 | 只读 provider、失败可降级 | Preview/Metadata/EventSource 边界已定义 | L3 | 基础系统打开/显示 | Quick Look、Spotlight、Vision/PDFKit、Windows provider 未实现 |

## 技术选择说明

- 当前汉明距离阈值不超过 8。64 位哈希切为 21/21/22 位三段；若总距离 ≤8，至少一段距离 ≤2，因此枚举每段 0–2 位邻域并以完整哈希过滤可保证召回。Criterion 基线见 [性能基线](./PERFORMANCE_BASELINE_2026.md)；HNSW 只留给未来高维 embedding。
- SQLite 使用 WAL 改善读写并发，FTS5 负责文本检索；写文件仍由带意图日志和发布验证的操作核心完成，而不是由 UI 或模型直接执行。
- 内容身份与路径分离是后续架构演进方向；当前稳定文件 id、路径历史和 generation 已覆盖安全操作所需边界，但未宣称达到跨设备同步系统的内容寻址模型。
- View Transition 是渐进增强，所有状态更新都有同步回退；系统或应用开启“减少动态效果”时禁用非必要动画。

## 尚未通过的发布门

1. 10 万文件扫描/搜索/重复分析；64 位候选已完成 10k/100k Criterion 和 1M 一次性规模门禁，但 1M 用时 53.2 s，仍需优化和 RSS 测量；
2. 真实文本/图片标注集上的 precision/recall、危险误报率和阈值校准；评估 CLI、precision 门禁与示例管线已完成；
3. 断电、真实跨卷、权限变化和外置盘拔出故障注入；当前已完成中途目标冲突和符号链接目录逃逸注入；
4. 系统选择器自动化与复制/移动/废纸篓操作 E2E；当前真实资料库、算法报告及重命名执行/撤销 5 项已通过；
5. macOS 签名、公证、升级和回滚安装测试。

这些项目是明确的下一阶段，不应在 README 或进度文档中写成已完成。
