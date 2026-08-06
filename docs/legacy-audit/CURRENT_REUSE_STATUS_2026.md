# 旧版本能力复用状态（2026-07-30）

本文把旧项目 `/Volumes/FX-MD87/ai-file-manager` 的实际源码能力映射到当前 Rust + Tauri 主线。结论来自 Swift/macOS、C#/WinUI 源码和当前 Git 历史的逐项核对；“旧版可用”只表示旧代码确实存在，不等于可以直接合并。

## 已在新主线重构完成

| 能力 | 旧版实现 | 当前实现 | 裁决 |
| --- | --- | --- | --- |
| 原生文件夹选择 | `NSOpenPanel` / Windows FolderPicker | Tauri 系统选择器、资料库授权边界 | 保留交互，重写实现 |
| 文件浏览、搜索、排序、列表/网格 | SwiftUI / WinUI 页面本地状态 | SQLite 索引、稳定分页、FTS5 与统一 Web/App UI | 保留产品能力，重写数据链路 |
| 文件变化监听 | macOS FSEvents | 事件合并、脏目录对账、generation 保护 | 保留平台能力，重写一致性逻辑 |
| 打开文件、在访达显示 | AppKit / Shell API | 平台命令层，参数化启动且不经过 shell | 保留平台能力，强化边界 |
| 大文件与清理建议 | 旧版大小/时间/下载目录规则 | 只读元数据分析；大文件、长期未修改、临时文件、陈旧下载四类解释 | 保留需求，算法与 UI 已重写 |
| 重复分析 | 旧版直接扫描并清理 | 分层 BLAKE3、持久缓存、稳定保留解释、只读报告 | 旧算法废弃，新算法完成 |
| 相似分析 | 旧版简单相似判断 | SimHash/MinHash、dHash/pHash、MIH 候选、只读报告 | 旧算法废弃，新算法已进入校准阶段 |
| 整理、重命名、移动、复制、隔离区与撤销 | 旧版直接文件 API、内存撤销 | 计划、预检、持久意图日志、发布验证、恢复审计与安全撤销 | 旧执行代码废弃，新事务链路完成 |

新加入的“清理建议”不会自动删除、移动或重命名文件；页面只允许查看候选和在系统文件夹中定位。候选规则具有固定边界、原因标签和单元测试，未来若接入清理操作，必须复用现有受控隔离区事务链路。

## 下一批可复用的系统能力

| 优先级 | 能力 | 旧版证据 | 新实现要求 |
| --- | --- | --- | --- |
| P1 | Quick Look | SwiftUI `.quickLookPreview` | 独立只读 `PreviewProvider`，失败回退当前安全文本预览 |
| P1 | 分类导航 | 旧版类别侧栏与分类结果 | 使用当前 `guixu-analysis` 分类契约，不复制旧分类引擎 |
| P2 | Spotlight / PDFKit / Vision 元数据 | `FileFeatureExtractor.swift` | 可选 `MetadataProvider`，限时、限内存、失败不阻断扫描 |
| P2 | 本地模型 provider | Ollama / oMLX 设置与服务层 | 明确离线/可选状态、超时与模型版本；模型只建议，不能直接写文件 |
| P3 | Windows 平台适配 | WinUI 选择器、Shell 能力 | 与 macOS 使用同一命令契约和事务核心，补 Windows 真机门禁 |

## 必须淘汰或完整重写

- Swift/C# 中直接 `move`、`delete`、`rename` 的整理与撤销代码；
- Windows 重复清理中的 `File.Delete`、吞异常和永久删除路径；
- 只存在内存中的撤销记录，以及未做身份、快照、权限和冲突预检的批处理；
- 旧 `ClassificationEngine`、`DeduplicationEngine`、`SmartNamingEngine` 的算法实现；
- 硬编码模型在线状态、演示统计和指向占位页的导航；
- 文档声称但源码不存在的 Rust FFI、Qdrant、Finder/Shell Extension 与云模型集成。

## 合并判断

旧项目适合提供产品需求、平台交互和失败场景样本，不适合作为代码基底整体合并。后续顺序保持为：先接只读平台 provider，再建立真实数据基准与阈值，最后才让新算法生成受控操作计划。任何写操作都不得绕过当前“计划 → 预检 → 持久日志 → 执行 → 恢复/撤销”主链路。
