# 旧项目到新核心的迁移矩阵

裁决含义：

- **保留**：保留产品行为或交互，不直接复制实现。
- **适配**：保留平台专属实现，收口到新核心接口。
- **重写**：需求成立，但旧实现不满足安全、性能或一致性要求。
- **废弃**：入口无效、重复或与当前架构冲突。
- **补建**：旧文档声明但源码缺失。

| 能力 | macOS 旧实现 | Windows 旧实现 | 裁决 | 进入新项目的方式 |
|---|---|---|---|---|
| 原生文件夹选择 | SwiftUI/AppKit 入口 | `FolderPicker` | 适配 | 平台层返回路径/授权句柄，UI 禁止手输路径作为主流程 |
| 目录扫描 | 枚举 + 批量入库 | 枚举 + 批量入库 | 重写 | 使用 `guixu-indexer`，可取消、增量、背压、统一忽略规则 |
| 文件变化监听 | FSEvents | `FileSystemWatcher` | 适配 | 平台事件归一化，索引器消抖、合并、重扫兜底 |
| SQLite/FTS 搜索 | 原生 SQLite + FTS5 | Microsoft.Data.Sqlite + FTS5 | 重写 | 统一由 `guixu-storage` 管理 schema、迁移、事务和查询转义 |
| 文件网格/筛选 | 已实现 | 已实现 | 保留 | UI 组件化后接新查询 API |
| Spotlight/PDF/Vision/OCR 特征 | 已实现 | 缺失 | 适配 | macOS provider；输出平台无关 `ExtractedFeature` |
| 规则分类 | 已实现 | 已实现 | 重写 | 旧规则只做测试样本；算法进入 `guixu-analysis` |
| 本地 AI 分类/命名 | oMLX/Ollama 兼容 HTTP | oMLX/Ollama 兼容 HTTP | 重写 | 可选 provider、显式隐私边界、超时/取消/结构化输出 |
| 精确重复检测 | SHA-256 | SHA-256 | 重写 | 大小分桶→快速指纹→流式强哈希→稳定分组 |
| 相似图片检测 | 感知哈希 | 感知哈希 | 重写 | 解码归一化、方向处理、dHash/pHash、候选索引与阈值测试 |
| 整理预览 | UI 有入口 | Dashboard 有操作 | 重写 | `OperationPlan` 只读生成，展示来源、目标、理由与冲突 |
| 批量移动 | 直接 `moveItem` | 直接 `File.Move` | 重写 | 事务执行器；跨卷 copy+fsync+verify+remove；逐步日志 |
| 冲突处理 | 自动 `_1` 后缀 | 遇到存在即跳过 | 重写 | 明确策略：跳过/重命名/替换；默认不覆盖 |
| 撤销/恢复 | 仅内存 move 日志 | 缺失 | 重写 | 持久 journal、幂等恢复、重启后可见、部分回滚 |
| 重命名 | 直接移动，冲突静默返回 | 入口不完整 | 重写 | 名称校验、预览、批量冲突检查、事务执行 |
| 删除重复文件 | 移到废纸篓 | `File.Delete` 永久删除 | 重写 | 默认移入系统废纸篓或隔离区；执行前逐项选择；可恢复 |
| 清理建议 | 大小/年龄/后缀启发式 | 不完整 | 保留 | 只给证据与建议，不自动删除 |
| Organize 独立页面 | ContentView 内组合 | 导航到 Dashboard 的 TODO | 废弃 | 建立统一计划页，不迁移占位导航 |
| 数据库加密 | 未落实 | 仅配置布尔值 | 补建 | 明确威胁模型后选择 SQLCipher/系统密钥；未实现前不展示开关 |
| Rust FFI 共享核心 | 文档声明，源码缺失 | 文档声明，源码缺失 | 废弃旧方案 | 使用现有 Rust workspace + Tauri 命令边界，不伪造 FFI 状态 |
| Qdrant 向量索引 | 文档声明，源码缺失 | 文档声明，源码缺失 | 延后 | 先做可解释关键词/FTS；语义索引单独做容量与隐私评估 |
| Finder/Shell 扩展 | 文档声明，源码缺失 | 文档声明，源码缺失 | 延后补建 | 核心稳定后作为独立平台插件，不进入当前关键路径 |

## 文件级裁决

### macOS

- `AIFileManagerApp.swift`：保留生命周期意图，重接新状态容器。
- `Models.swift`：仅作为兼容输入；领域模型以 `guixu-domain` 为准。
- `FileManagerService.swift`：拆解并重写；禁止作为单例巨石迁移。
- `FileFeatureExtractor.swift`：保留平台能力，适配为只读 provider。
- `ClassificationEngine.swift`、`DeduplicationEngine.swift`、`SmartNamingEngine.swift`：旧算法全部作为行为样本，后续重写。
- `OllamaService.swift`：保留协议认知，重写可靠性和隐私边界。
- `ContentView.swift`：按页面/状态/组件拆分，不整体复制。
- `DesignSystem.swift`：可选择性迁移无副作用的 token 和组件。

### Windows

- `MainWindow.xaml(.cs)`：保留导航信息架构，删除占位路由。
- `DashboardPage.xaml(.cs)`：保留 FolderPicker 交互；整理动作改接计划 API。
- `FileGridPage.xaml(.cs)`：保留视图意图；查询和错误处理重写。
- `DuplicatePage.xaml(.cs)`：保留分组展示；永久删除逻辑禁止迁移。
- `SettingsPage.xaml(.cs)`：保留目录选择；删除未实现的加密承诺。
- `Models.cs`：仅用于兼容映射；不作为新领域模型。
- 五个 Services：平台能力可适配，核心算法和磁盘写入全部重写。
