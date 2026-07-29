# 旧项目合并边界

本文件定义 `/Volumes/FX-MD87/ai-file-manager` 与当前“归序”项目的合并范围。目标是复用成熟的产品交互和操作系统能力，同时避免把旧项目中不可恢复、不可审计的文件操作逻辑带入新核心。

## 立即保留并重构

| 旧能力 | 来源 | 新归属 | 合并方式 |
| --- | --- | --- | --- |
| 分栏导航、文件列表/网格、空状态 | macOS `ContentView.swift`、Windows Pages | Web 页面组件 | 只迁移信息架构和交互，不复制 2,244 行单文件视图 |
| 文件夹选择器 | macOS `NSOpenPanel`、Windows Picker | Tauri 平台适配与命令层 | 平台授权后登记资料库，不开放手输路径主流程 |
| Quick Look / 系统预览 | macOS QuickLook | 后续平台预览 provider | 只读能力，与整理事务隔离；当前尚未接入 |
| Spotlight、PDFKit、Vision 元数据 | `FileFeatureExtractor.swift` | `PlatformMetadataProvider` | 作为可选特征提供者；失败不影响基础扫描 |
| FSEvents | `FileManagerService.swift` | `DirectoryEventSource` | 只产生“路径已变化”事件，稳定性、规则与执行仍由新核心决定 |
| 搜索、排序、视图模式 | macOS/Windows 文件页 | 新索引与页面状态 | 统一字段和分页契约后重写 |
| 本地模型连接设置 | macOS/Windows Settings、`OllamaService` | 可选 AI Provider | 不允许模型直接移动、删除或重命名文件 |

## 禁止直接合并

- `FileManagerService.swift` / `FileManagerService.cs` 的移动、删除、撤销和自动整理实现；
- `ClassificationEngine`、`DeduplicationEngine`、`SmartNamingEngine` 的旧算法实现；
- 仅存在于内存中的撤销栈、直接 `moveItem`/`Move`、未经预检的批量写入；
- Windows 页面中的演示统计、硬编码模型状态和未完成的整理入口；
- 旧构建产物、用户状态、`.build`、`bin`、`obj` 和 Xcode 用户目录。

## 新接口边界

```text
Web 页面 / Tauri 桌面壳
  ├─ FolderAccessProvider  （选择与授权，只返回路径/书签）
  ├─ PreviewProvider       （只读系统预览）
  ├─ MetadataProvider      （只读特征，可降级）
  └─ DirectoryEventSource  （只发变化通知）
             ↓
       Tauri Commands
             ↓
  新算法核心（计划 → 预检 → 事务日志 → 执行 → 撤销）
```

任何平台适配器都不得直接执行文件移动。所有改变文件系统的操作必须经过新核心生成计划，并持久化到 SQLite 操作日志。

## 合并顺序与验证门

1. 页面路由与状态机：每次只能显示一个主页面；浏览器逐入口验证。
2. 拆分旧 UI 信息架构：先接文件浏览、搜索和预览的只读接口；做空目录、大目录和权限拒绝测试。
3. 接入平台能力：先完成文件夹授权、打开和显示；再接 Quick Look、Spotlight；每个适配器都提供禁用/失败降级路径。
4. 接入目录事件源：事件只触发重新扫描；验证突发事件合并、重命名、删除和休眠恢复。
5. 最后重写算法：分类、相似重复、智能命名和批处理分别建立基准集、正确率指标与事务安全测试后替换。

当前状态：页面主路由、文件夹授权、文件浏览/搜索/预览、打开/显示、后台索引和 FSEvents 对账已经接入；Quick Look、Spotlight、PDF/Vision 与 Windows provider 尚未完成。`guixu-analysis` 已有带持久缓存的分层精确重复、稳定保留评分、文本 SimHash + MinHash 复核和 EXIF 归一化图片 dHash + DCT pHash 复核；相似能力仍保持只读，待真实标注集校准阈值。早期 Python/Web 原型已集中归档到 `legacy/python-prototype`，不再作为启动入口；旧算法继续维持“禁止直接复制”边界。完整证据见 [交叉验证矩阵](./CROSS_VALIDATION_2026.md)，顺序与验收标准见 [整体方案](./MASTER_PLAN.md)。
