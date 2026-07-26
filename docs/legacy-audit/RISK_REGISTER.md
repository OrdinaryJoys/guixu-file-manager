# 旧项目风险登记表

| 等级 | 风险 | 证据 | 处理 |
|---|---|---|---|
| P0 | Windows 重复清理永久删除并吞掉异常 | `DuplicatePage.xaml.cs:105` 调用 `File.Delete`，空 `catch` | 禁止迁移；改用废纸篓/隔离区与逐项结果 |
| P0 | 整理操作缺少持久事务 | macOS `operationLog` 仅内存；Windows 无撤销 | 新执行器必须先写 journal，再修改文件 |
| P0 | 数据库与文件系统可能分叉 | 直接移动/删除后分别更新内存和数据库 | 操作状态机 + 重启恢复 + 索引重放 |
| P1 | macOS 撤销可能覆盖后来出现的同名文件 | 回移前未验证目标身份/冲突，仅尝试 `moveItem` | 文件身份、指纹、目标存在检查；默认停止 |
| P1 | Windows 整理结果未同步模型/数据库 | `OrganizeFilesAsync` 只执行 `File.Move` | 不修补旧服务，由统一执行器发出提交事件 |
| P1 | FTS 查询直接送入 `MATCH` | 两端均未建立统一的查询解析/转义层 | 新存储层解析用户查询并返回结构化错误 |
| P1 | 配置声称数据库加密但未见加密实现 | `Models.cs:146` 与 Settings 开关 | UI 暂不展示；实现并验证后再开放 |
| P1 | 文档与实现严重不一致 | 架构文档声称 Rust/Qdrant/FFI/扩展，源码缺失 | 文档以可执行清单和测试为准 |
| P2 | Windows Organize 导航是占位页 | `MainWindow.xaml.cs:75` | 新计划页完成后再开放入口 |
| P2 | 大服务和大视图难以验证 | macOS `ContentView` 约 2,244 行；服务约 995 行 | 按用例和副作用边界拆分 |
| P2 | 旧项目没有自动化测试 | 无 Swift/C# 测试源码 | 先建特征测试，不以 README 声明作为验收 |
| P2 | 空 `catch` 隐藏权限、锁定和 I/O 错误 | Windows 多处存在 `catch { }` | 所有批处理返回逐项成功/失败/跳过原因 |

## 写操作状态机

新核心统一采用：

`Draft → Validated → Previewed → Committed → Executing → Completed`

异常路径为 `Executing → Interrupted/PartiallyCompleted → Resuming/RollingBack → Completed/NeedsAttention`。任何 UI 都不能绕过 `Previewed` 和 `Committed` 直接进入执行。
