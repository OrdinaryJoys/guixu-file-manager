# 归序性能与阈值基线（2026-07-30）

本文只记录可复现的已测结果，不把合成微基准等同于真实资料库性能。

## 64 位相似候选索引

实现位于 `guixu-analysis::find_similarity_candidates`。64 位哈希按 21/21/22 位切成三段，每段枚举汉明距离 0–2 的邻域，再用完整 64 位距离过滤。由于总距离不超过 8 时至少有一段距离不超过 2，该索引不会漏掉阈值内候选；单元测试同时与 512 项穷举结果比较。

测试环境：macOS 15.7.7、arm64、Rust 1.85、Criterion 0.7.0，输入为 SplitMix64 生成的确定性均匀哈希，阈值为 8。Criterion 每个规模采集 10 个样本：

| 输入规模 | 95% 估计区间 | 中位估计 | 吞吐中位估计 |
| --- | --- | --- | --- |
| 10,000 | 97.609–98.908 ms | 98.024 ms | 102.02 K elements/s |
| 100,000 | 1.2948–1.3601 s | 1.3207 s | 75.719 K elements/s |

复现命令：

```bash
cargo bench -p guixu-analysis --bench similarity
```

这些数字衡量候选索引本身，不包含文件读取、文本规范化、图片解码、SQLite I/O 或任务调度。均匀随机哈希的桶分布也不能代表相机连拍、模板文档等高聚集真实数据。下一基线必须补充 1M 高聚集分布、峰值 RSS 与 p50/p95。

## 全量扫描扩展性（2026-08-03）

实现位于 `guixu-indexer::reconcile_snapshot`（含 SQLite 写入与 FTS 同步）。输入由 `test-fixtures/generate-scale.mjs` 用固定种子生成（SplitMix64 确定性伪随机）；release 构建、全新数据库（含 12 个迁移）、单次全量扫描。

测试环境：macOS 15.7.7、arm64、Rust 1.85、release。10k 为混合大小分布（70% <2KiB / 20% 2-64KiB / 8% 64KiB-1MiB / 2% 1-8MiB），100k 为 tiny 分布（全部 <2KiB，磁盘受限）：

| 文件数 | 目录数 | 已遍历条目 | 耗时 | 吞吐 |
| --- | --- | --- | --- | --- |
| 10,000 | 7,421 | 22,446 | 1.75 s | 12.8 K files/s |
| 100,000 | 60,119 | 215,909 | 26.6 s | 8.1 K files/s |

扩展性系数：10 倍文件数 → 15.2 倍耗时，低于 20 倍试运行预算。复现命令：

```bash
node test-fixtures/generate-scale.mjs e2e/.artifacts/scale-10k 10000 20260803
node test-fixtures/generate-scale.mjs e2e/.artifacts/scale-100k 100000 20260803 tiny
GUIXU_SCALE_DIR=e2e/.artifacts/scale-10k cargo test --release -p guixu-indexer --lib -- --ignored scale_scan --nocapture
```

两种分布不完全可比（10k 含大文件、100k 全小文件），但扫描主成本是元数据遍历与 SQLite 写入。**2026-08-03 同分布复测确认**：tiny 分布 10k 扫描 1.75 s（12.9 K files/s），与混合分布几乎一致——文件大小对扫描耗时无影响，15–17× 扩展系数是真实特征（FTS 同步与 SQLite B-tree 增长），仍在 20× 试运行预算内。

## 100k 端到端计时（2026-08-03）

同一索引（tiny 100k 全量扫描后）上的搜索与分页计时，release 构建、单次运行：

| 操作 | 10k | 100k | 预算对照（TEST_STRATEGY §6） |
| --- | --- | --- | --- |
| 全量扫描 | 1.82 s | 30.5 s | 无变化重扫扩展 <20×：16.8× ✅ |
| 搜索 `报告`（2 字，LIKE 回退） | 82 µs | 523 µs | — |
| 搜索 `2026`（trigram） | 2.3 ms | 87.9 ms | 100k 热搜索 p95 <200 ms：87.9 ms ✅ |
| 搜索 `contract`（unicode61） | 2.6 ms | 41.8 ms | ✅ |
| 搜索 `ext:md`（结构化过滤） | 85 µs | 126 µs | ✅ |
| 全量分页（100/页） | 101 页 8 ms | 1001 页 104 ms | — |

复现命令：`GUIXU_SCALE_DIR=e2e/.artifacts/scale-100k cargo test --release -p guixu-indexer --lib -- --ignored scale_scan --nocapture`（输出 `SCALE_SCAN`/`SCALE_SEARCH`/`SCALE_PAGING` 行）。

100k 搜索最差 87.9 ms，低于 200 ms 试运行预算；trigram 与 unicode61 的 16–38× 规模增长来自 FTS 索引体积与 bm25 排序，需在冻结预算前以 p50/p95 多轮复测。此数字仍不包含真实资料库目录聚集或 UI 轮询叠加。

## 100k 精确重复分析端到端（2026-08-03）

tiny 100k（全部唯一内容）上的 `find_exact_duplicates`，release 单次运行：

| 指标 | 值 |
| --- | --- |
| 输入文件 | 100,000 |
| 快速指纹（跳过全量哈希） | 99,855（99.9%） |
| 完整哈希 | 0（快速指纹已完全区分） |
| 重复组 | **0（零危险误报）** |
| 耗时 | 9.59 s（10.4 K files/s） |

复现：`GUIXU_SCALE_DIR=e2e/.artifacts/scale-100k cargo test --release -p guixu-analysis --lib -- --ignored scale_duplicates --nocapture`。

测试同时作为算法回归门：内容全部唯一的 fixture 不允许出现任何重复组。生成器曾因 SplitMix64 `% 256` 低 8 位周期（256）产生跨文件相同内容段，算法正确检出了这些真实重复；生成器已改为按文件名派生的 SHA-256 内容填充，重新生成后零重复并零假阳性。**该缺陷被算法端到端测试捕获，验证了零危险误报门禁的有效性。**

### 1M 一次性规模门禁

`cargo run --release -p guixu-analysis --example scale_similarity -- 1000000` 在同一环境完成 1,000,000 个确定性哈希，找到 161 对距离阈值内候选，耗时 53,200 ms，吞吐 18,796.85 elements/s。它证明百万级路径可完成，但不是统计基准，也未测峰值 RSS；53.2 秒仍属于下一轮并行化/内存布局优化对象。

## 双阶段相似验证

- 文本：SimHash 距离 ≤8 进入候选，32 分量字符五元组 MinHash 复核；当前产品门限为 MinHash ≥0.25。
- 图片：dHash 距离 ≤8 进入候选，DCT pHash 复核；当前产品门限为 pHash 距离 ≤12。
- 报告分别保存主距离、二次证据、最终加权分、特征耗时和候选耗时。

`evaluate_similarity_threshold` 输出 TP/FP/TN/FN、precision、recall 与 F1；`select_similarity_threshold` 可在调用方给出的门限网格上选择 F1 最优且更保守的并列项。当前测试只证明计算与选择规则正确，不能证明产品门限已校准。真实标注集应包含真重复、近重复、相关但不可合并、完全不相关四类，并按来源隔离训练/验证集合。

`evaluate_thresholds` CLI 强制区分 `train` 与 `validation`，并可要求最低 precision；验证集报告同时包含 FPR 和 accuracy。示例：

```bash
cargo run -p guixu-analysis --example evaluate_thresholds -- \
  crates/guixu-analysis/tests/fixtures/threshold_labels_example.csv 0.90
```

仓库 CSV 是评估管线 fixture，不是产品准确率证据。真实门限必须用独立来源的人工标注数据重新训练和验证。
