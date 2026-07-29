# 旧项目基线与构建验证

审计日期：2026-07-22（Asia/Shanghai）

## 源码规模

旧目录约 9,248 行源码。可迁移源码清单如下；`.build`、`bin`、`obj`、DerivedData 和 `.DS_Store` 不属于源码基线。

### macOS

- `AIFileManagerApp.swift`
- `Models.swift`
- `Services/ClassificationEngine.swift`
- `Services/DeduplicationEngine.swift`
- `Services/FileFeatureExtractor.swift`
- `Services/FileManagerService.swift`
- `Services/OllamaService.swift`
- `Services/SmartNamingEngine.swift`
- `Views/ContentView.swift`
- `Views/DesignSystem.swift`
- `AIFileManager.entitlements`

其中 `ContentView.swift` 约 2,244 行，`FileManagerService.swift` 约 995 行，已形成明显的 UI 巨石与服务巨石。

### Windows

- `App.xaml` / `App.xaml.cs`
- `MainWindow.xaml` / `MainWindow.xaml.cs`
- `Models/Models.cs`
- `Pages/DashboardPage.xaml` / `.xaml.cs`
- `Pages/DuplicatePage.xaml` / `.xaml.cs`
- `Pages/FileGridPage.xaml` / `.xaml.cs`
- `Pages/SettingsPage.xaml` / `.xaml.cs`
- `Services/ClassificationEngine.cs`
- `Services/DeduplicationEngine.cs`
- `Services/FileManagerService.cs`
- `Services/OllamaService.cs`
- `Services/SmartNamingEngine.cs`

## macOS 干净构建

使用旧源码、独立临时 DerivedData、关闭签名：

```sh
xcodebuild \
  -project /Volumes/FX-MD87/ai-file-manager/macos/AIFileManager/AIFileManager.xcodeproj \
  -scheme AIFileManager \
  -configuration Debug \
  -derivedDataPath /tmp/guixu-old-macos-build-escalated \
  CODE_SIGNING_ALLOWED=NO \
  build
```

结果：`BUILD SUCCEEDED`。当前安装的 Xcode/SDK 能编译该源码。构建只验证编译与链接，不证明运行时正确、数据安全或功能完整。

第一次在受限执行沙箱内运行时，Xcode 的内部 `sandbox-exec` 无法再次应用沙箱，Swift 预览宏随之报错；同一命令在系统编译边界外成功，因此该次失败不判定为源码缺陷。

## 测试现状

- 未发现 Swift 测试源码。
- 未发现 C# 测试项目或测试源码。
- `Package.swift` 声明 `AIFileManagerTests`，但仓库中没有相应测试目录。
- 旧 README 中“全部运行问题已修复”的声明没有自动化测试证据支撑。

## Windows 构建状态

已确认入口为 `windows/AIFileManager/AIFileManager.csproj`，但当前 macOS 构建机没有安装 .NET SDK（`dotnet` 不存在），因此本轮未执行 Windows/WinUI 编译。该项状态是“环境未验证”，不是“构建失败”。后续应在 Windows 11 + .NET 8 + Windows App SDK 环境中执行 `dotnet restore`、`dotnet build` 和最小启动测试。

## 新主线验证环境说明

Rust/Homebrew 工具链已经修复并恢复验证。只读资料库概览、持久整理/重命名/复制/移动/可恢复删除计划、事务执行、操作历史、撤销、设置、持久哈希缓存、重复保留评分与启动恢复审计均已通过 workspace 测试；截至 2026-07-28，当前 Rust 测试为 68/68，Python 临时目录回归测试为 13/13。

另使用完全位于 `/tmp` 的两文件资料库完成了真实桌面端到端验证：原生文件夹选择 → 扫描 → 勾选 → 计划预览 → 确认执行 → 历史记录 → 撤销。文件执行后进入 `归序整理/<类别>/`，撤销后回到原路径，索引路径与概览统计同步。

## 可重复校验

迁移某个文件前，用以下只读命令记录快照；输出保存到新仓库的审计附件，不写回旧目录：

```sh
find /Volumes/FX-MD87/ai-file-manager/macos/AIFileManager/AIFileManager \
  /Volumes/FX-MD87/ai-file-manager/windows/AIFileManager \
  -type f \( -name '*.swift' -o -name '*.cs' -o -name '*.xaml' \) \
  -exec shasum -a 256 {} \;
```
