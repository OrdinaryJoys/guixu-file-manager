# 归序原生窗口 E2E

该目录使用 WebdriverIO 9 与 `@wdio/tauri-service` 1.2.0 的嵌入式 WebDriver，在真实 macOS WKWebView/Tauri 进程中执行基础交互。测试插件只通过 Cargo `e2e` feature 编译，不进入普通或发布构建。

```bash
cd e2e
npm install
npm test
```

测试启动前会在 `e2e/.artifacts` 一次性创建四文件隔离资料库；仅 `e2e` Cargo feature 会读取 `GUIXU_E2E_LIBRARY` 并注册该目录，普通与发布构建不存在此入口。当前 5 个场景覆盖：真实扫描、搜索与文本预览、智能文件夹、整理计划预览、精确重复与相似文本后台报告、真实重命名/历史/安全撤销，以及设置即时生效与重新读取。截图和数据库均写入隔离目录，不会使用个人应用数据。

注意：`@wdio/tauri-service` 1.2.0 固定依赖的 `@wdio/native-utils` 2.4.0 缺少其实际导入的符号，因此 `package.json` 明确覆盖为 2.5.0。当前服务还会在嵌入式驱动已正常运行时输出一次“tauri-driver not found”外部驱动诊断，以及结束阶段的 mock-store 警告；两者不影响嵌入式会话，成功门以退出码和 spec 结果为准。

所有 npm 包都是开发依赖，`npm audit --omit=dev` 为 0。完整开发依赖图当前仍报告 24 个 WebdriverIO/Mocha 传递依赖公告；它们不会打入 App，但应随上游版本修复持续复查，不能用破坏性 `npm audit fix --force` 静默降级测试栈。
