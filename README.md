# 归序 / Guixu

归序是一款离线优先的本地文件管理与整理工具。当前主线使用 Rust + Tauri 2，提供系统文件夹选择、后台索引、搜索与智能文件夹、文件预览、安全整理计划、持久操作历史、撤销和只读精确重复分析。

在 macOS 页面点击“选择文件夹”会直接打开系统目录选择器；选定后自动扫描并生成预览，不需要手动输入路径。

## 启动与验证

需要 Rust 1.85 或更高版本，以及 Tauri 2 对应的系统构建依赖。新核心位于 `crates/`，桌面入口位于 `apps/desktop/`。

```bash
cargo test --workspace
cargo check --workspace
cargo run -p guixu-desktop
```

当前已完成：

- macOS 原生目录选择、流式索引、稳定文件身份、路径重关联和游标分页；
- SQLite WAL/FULL、持久任务租约、操作意图日志、崩溃恢复与禁止覆盖发布；
- 同卷排他原子移动，跨卷保留元数据复制、BLAKE3 校验和发布后验证；
- FTS5 Unicode/Trigram 搜索、结构化查询、智能文件夹、安全文本预览、任务中心和持久化设置；
- FSEvents 脏目录合并与权威快照对账；
- 带快照失效规则和持久哈希缓存的只读分层精确重复、魔数/容器分类和安全命名基础算法；
- 列表/网格、排序、多选、键盘导航、批量栏、打开及“在访达中显示”。

批量重命名、安全复制、通用移动与可恢复删除已完成“参数/原生目录选择 → 安全预检 → 计划确认 → 持久执行 → 历史 → 撤销”闭环；持久哈希缓存已接入重复分析，相似文本/图片基础 provider 已接入统一持久任务和只读 UI，阈值基准与平台增强仍在后续里程碑中。完整状态、算法方案与验收门见 [整体方案](docs/MASTER_PLAN.md)。

当前里程碑和已知限制可快速查看 [开发进度](docs/DEVELOPMENT_STATUS.md)，界面组件和视觉约束见 [UI 设计系统](docs/UI_DESIGN_SYSTEM.md)，相似内容、端侧模型和混合检索选型见 [2026 算法与技术架构](docs/ALGORITHM_DESIGN_2026.md)。

## 核心安全约束

- UI 只以资料库 ID 和文件 ID 请求能力，不能操作任意路径；
- 整理前预览，当前计划 15 分钟有效，执行前再次校验身份、快照和目标；
- SQLite 逐文件事务日志、崩溃状态恢复、完整撤销；
- 同卷使用排他移动，跨卷采用复制、落盘、BLAKE3 校验和排他发布；
- 默认不覆盖现有目标，不跟随符号链接；
- 启动时只审计中断操作，歧义状态不会自动移动或删除文件；
- 重复分析提供可解释保留评分；清理必须逐项勾选并进入受控废纸篓计划，不提供永久删除入口。

## 数据与隐私

主线数据库 `guixu.sqlite3` 默认位于系统分配的应用数据目录；开发或测试可通过 `GUIXU_DATA_DIR` 指定其他目录。文件内容默认只在本机按需读取，不上传到外部服务。

## 验证

```bash
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
node --check apps/desktop/web/app.js
node tools/verify_frontend_contract.mjs
```

截至 2026-07-29，Rust workspace 为 87 项测试通过，并已增加 Criterion 相似候选基准和 macOS 嵌入式 WebDriver 原生窗口 E2E。浏览器演示可用 `?demo=1` 快速验证页面交互；Web 演示与 Tauri App 共享唯一的 `apps/desktop/web` 前端资源、相似内容报告契约和任务状态。原根目录 Python 页面已移入 `legacy/python-prototype`，只保留作审计基线。完整的代码、Web 和原生 App 证据见 [交叉验证矩阵](docs/CROSS_VALIDATION_2026.md)；正式签名、公证和发布仍属于后续真机验收项。

## 旧项目合并

旧 Python、macOS 和 Windows 项目只作为行为与风险基线。旧算法和不安全磁盘写入不会直接复制；哪些能力保留、适配、重写或延后见 [旧项目合并边界](docs/LEGACY_MERGE_BOUNDARY.md) 与 [旧项目审计](docs/legacy-audit/README.md)。
