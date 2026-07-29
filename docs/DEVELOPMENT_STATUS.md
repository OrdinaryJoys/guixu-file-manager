# 归序开发进度

> 状态日期：2026-07-30
>
> 当前版本：0.1.0（开发中）
> 主线：Rust + Tauri 2

本文用于快速查看当前完成度。产品边界、算法细节和完整验收标准以 [整体方案](./MASTER_PLAN.md) 为准。

## 总体进度

| 阶段 | 状态 | 交付内容 |
| --- | --- | --- |
| M0 基线与页面修复 | 完成 | 响应式布局、焦点与滚动、OOXML 容器分类修复 |
| M1 后台任务闭环 | 完成 | 后台扫描、进度、暂停/恢复/取消、事件推送、目录对账 |
| M2 文件浏览交互 | 完成 | 列表/网格、排序、多选、键盘导航、预览、打开和显示 |
| M3 搜索与智能文件夹 | 完成 | Unicode/Trigram 搜索、结构化条件、智能文件夹持久化 |
| M4 通用文件操作 | 完成 | 批量重命名、安全复制、通用移动与受控隔离区删除均已形成可恢复闭环 |
| M5 算法生产化 | 进行中 | M5.1–M5.3 完成；M5.4 已形成双阶段链路、精确 MIH、precision 安全阈值工具、10k/100k 统计基线和 1M 规模门禁，真实标注集校准待推进 |
| M6 平台增强与发布 | 待开始 | Quick Look、Spotlight/Vision、Windows provider、安装发布 |

## 当前可用能力

- 通过 macOS 系统选择器建立本地资料库；
- 流式扫描、稳定文件身份、游标分页和 FSEvents 对账；
- 文件列表/网格、排序、选择、文本预览、系统打开和访达显示；
- FTS5 Unicode/Trigram 搜索，以及 `ext:pdf`、`size:>10MB` 等结构化查询；
- 智能文件夹保存、执行和删除，以及长名称/查询摘要的稳定侧栏布局；
- 设置入口固定在侧栏底部并与业务导航分离；设置页面按通用、外观与显示、扫描与性能、本地数据分区，可配置恢复上次资料库、默认视图与排序、界面密度、大小/日期格式、路径与概览显示、任务中心、分页、刷新频率及减少动态效果，持久保存后立即生效；
- UI 已建立统一设计令牌和组件规范，资料库、设置、重复分析、历史、计划和任务中心共用文字、表面、边框、圆角、按钮、弹窗与状态语义；规范见 [UI 设计系统](./UI_DESIGN_SYSTEM.md)；
- 侧栏提供大文件快捷筛选和只展示受控废纸篓记录的“可恢复文件”入口；
- 精确重复分析使用绑定稳定文件 ID、大小、修改/状态时间和算法版本的持久哈希缓存；报告区分本次读取与缓存命中；
- 设置“本地数据”页展示索引量、指纹/完整哈希缓存量，可重新扫描资料库并清理可重建缓存；
- 重复文件按修改时间、路径层级、副本名称和隐藏目录输出确定性保留分与解释；用户必须逐项选择清理对象，且每组至少保留一项；
- 重复清理复用 M4 受控废纸篓的计划预览、预检、执行、历史和撤销链路，不开放永久删除；
- 精确重复分析已进入持久后台队列，展示真实进度并支持暂停、恢复和取消；中断时保留已计算哈希，完成报告跨重启持久化；
- 相似文本 SimHash 与相似图片 dHash 共用三段 Multi-Index Hashing 候选索引；每段枚举 0–2 位邻域，可完整召回汉明距离不超过 8 的候选，再分别以 32 分量字符五元组 MinHash 和 DCT pHash 复核；
- 混合检索融合基础已实现：无标注时使用确定性 RRF，有本地校准样本时使用归一化凸组合，禁止直接混加 BM25 与余弦原始分数；
- schema 11 已持久化快照/模型版本绑定的文件特征、相似运行与候选边；相似文本和图片在 Web 演示与 Tauri App 使用同一报告契约、只读卡片 UI 和任务中心；
- 图片基础 provider 支持 JPG/JPEG/PNG/WebP，采用有尺寸与内存上限的一次解码，同时生成 64 位 dHash 与低频 DCT pHash；文本/图片分析均可暂停、恢复、取消，完成报告跨重启持久化；
- 选择式整理计划、执行前预检、持久操作日志、执行、历史和撤销；
- 只读精确重复分析，不提供永久删除入口；
- 基础魔数/容器分类、SimHash、dHash 和安全命名算法测试。
- 类型化 `Organize/Rename/Move/Copy/Trash` 计划与冲突策略持久化；
- 批量重命名只读计划：名称、权限边界、身份/快照、现有目标和批内冲突预检。
- 批量重命名页面、计划确认、持久执行、索引同步、操作历史和安全撤销。

## 已知限制

- **2026-07-29 全库审计与首轮修复已完成**：原始审计记录 68 项（高 8 / 中 21 / 低 39）；P0 数据安全、P1 核心任务生命周期、P2 关键锁竞争、P3 前端竞态均已落地并补测试，P4/P5 的平台增强与低危边界继续按 [REMEDIATION_PLAN](./REMEDIATION_PLAN.md) 跟踪；
- 受控隔离区当前不自动永久清空，保留策略是持续保留并由历史记录恢复；永久清理入口待独立安全评审；
- 相似文本/图片基础 provider 已接入产品并处理 EXIF 方向，pHash/MinHash 二次验证及阈值评估代码已完成；尚缺真实标注数据集的 precision/recall 校准和平台增强 provider；
- 精确重复评分仍需真实基准集校准；相似内容在评估完成前保持只读，不开放清理；
- 真实外置卷上的首次图片特征任务曾出现异常长等待，已确认最终结果与缓存正确；当前报告已区分特征阶段与候选阶段耗时，下一阶段继续拆分解码、数据库写入和任务调度并建立冷启动 p95 门槛；
- Quick Look、Spotlight、PDFKit/Vision 和 Windows 平台 provider 尚未接入；
- 正式签名、公证、安装器，以及真实“打开文件/在访达显示”的发布流程仍需真机验收；

## 当前验证基线

截至状态日期（以最新一次全量命令输出为准）：

- Rust workspace：89 项测试通过，0 项失败；
- `cargo fmt --all --check`：通过；
- 严格 Clippy：通过；
- `node --check apps/desktop/web/app.js`：通过；
- `node tools/verify_frontend_contract.mjs`：通过；
- `cargo bench -p guixu-analysis --bench similarity`：10k 为 98.024 ms 中位估计，100k 为 1.3207 s 中位估计（macOS 15.7.7 / arm64，确定性合成哈希）；1M 一次性规模门禁耗时 53.2 s；
- 阈值 CLI：支持 train/validation 分离、最低 precision 门禁，输出混淆矩阵、precision、recall、F1、FPR 与 accuracy；仓库示例标签仅验证管线，不冒充真实标注集；
- `cd e2e && npm test`：macOS 嵌入式 WebDriver 原生 App 5 项通过，覆盖真实资料库扫描、搜索/预览、智能文件夹、整理计划、重复/相似报告、真实重命名/历史/撤销及设置；
- `git diff --check`：通过；
- Web 与 Tauri App 共用同一套 `apps/desktop/web` 前端资源和命令契约，搜索、智能文件夹、设置、预览、相似内容和任务状态未维护第二份实现；
- GitHub Actions `Quality gates` 在 macOS 上持续执行 Rust、前端契约、生产依赖审计与原生 Tauri E2E；
- Tauri 官方 debug `.app` 已在 macOS 15.7.7 / Apple Silicon 真实运行：核心连接、真实索引、设置四分区、搜索、智能文件夹、文件预览、相似文本/图片和任务结果回填通过；真实拖拽到 900×620 最小尺寸验证无横向溢出，侧栏标签保持单行，窄窗口隐藏预览和修改时间列并保留最新两项任务。

```bash
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
node --check apps/desktop/web/app.js
node tools/verify_frontend_contract.mjs
cargo bench -p guixu-analysis --bench similarity
cargo run --release -p guixu-analysis --example scale_similarity -- 1000000
cargo run -p guixu-analysis --example evaluate_thresholds -- crates/guixu-analysis/tests/fixtures/threshold_labels_example.csv 0.90
cd e2e && npm test
```

模块级代码、算法、Web 和原生窗口证据以及仍未通过的发布门，统一记录在 [交叉验证矩阵](./CROSS_VALIDATION_2026.md)。

## 下一开发检查点

M4.1–M4.5、M5.1–M5.3 已完成；M5.4 已交付双阶段复核、EXIF 方向归一化、MIH 候选、precision 安全评估工具与 10k/100k/1M 基线。下一增量导入真实标注集并校准阈值，再接入 macOS Vision provider；完成准确率验收前不开放批量清理。

每完成一个增量，依次执行：单元测试 → 临时目录集成测试 → 页面交互验证 → 更新本文和主方案状态。
