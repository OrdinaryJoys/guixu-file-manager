# 归序算法与技术架构设计（2026）

> 状态：M5.4–M6 实施基线

## 1. 总体架构

归序采用“确定性核心 + 感知候选 + 可选语义增强”三级本地架构：

1. **安全层**：稳定文件身份、快照、大小、分层 BLAKE3 和完整哈希。只有这一层能够证明内容完全相同。
2. **感知层**：图片 dHash/pHash、文本 SimHash/MinHash、视频关键帧和音频指纹。只生成近重复候选。
3. **语义层**：平台原生特征或可选端侧 embedding。用于内容相关性和自然语言搜索，不作为清理证明。

写操作始终经过“候选 → 人工选择 → 安全计划 → 快照复检 → 可恢复执行”。模型分数不能绕过计划状态机。

## 2. 技术选择

### 2.1 图片

- 默认使用 64 位 dHash，并增加旋转/裁剪归一化与多尺度 pHash，覆盖截图、导出副本和轻微压缩。
- macOS 增强 provider 使用 Apple Vision `VNGenerateImageFeaturePrintRequest`；系统直接提供特征距离计算。
- 跨平台可选 MobileCLIP2 小模型，用于端侧图片语义和图文检索。官方模型族以 3–15ms 延迟、50–150M 参数为目标。
- SigLIP 2 改善多语言图文检索，但最小 ViT-B 仍约 86M 参数，因此作为可下载模型包，而不是基础安装依赖。
- DINOv2 用作视觉实例检索实验基线，不作为首发默认 provider。

参考：[Apple Vision Feature Print](https://developer.apple.com/documentation/vision/vnfeatureprintobservation)、[MobileCLIP2](https://machinelearning.apple.com/research/mobileclip2)、[SigLIP 2](https://arxiv.org/abs/2502.14786)、[DINOv2](https://dinov2.metademolab.com/)。

### 2.2 文本

默认层使用 Unicode NFKC、语言无关字符 shingles、SimHash 和 MinHash：SimHash 处理局部编辑，MinHash + LSH 处理长文档集合相似度和段落重排。PDF/Office 文本绑定文件快照与解析器版本。

语义增强采用可选 EmbeddingGemma provider。它面向端侧、覆盖 100 多种语言，并支持 Matryoshka 768/512/256/128 维输出。归序默认保存归一化 128 维向量用于召回，再以 256/768 维重排。量化内存仍接近 200MB，因此模型必须由用户明确下载。

参考：[EmbeddingGemma](https://deepmind.google/models/gemma/embeddinggemma/)、[模型卡](https://ai.google.dev/gemma/docs/embeddinggemma/model_card)、[Matryoshka Representation Learning](https://proceedings.neurips.cc/paper_files/paper/2022/hash/c32319f4868da7613d78af9993100e42-Abstract-Conference.html)。

### 2.3 候选索引

| 规模 | 索引 | 用途 |
| --- | --- | --- |
| 64 位二进制哈希 | 9 段倒排 LSH | 当前已实现；完整召回汉明距离 ≤8 |
| 向量少于 50k | 分块 SIMD 精确余弦 top-k | 稳定结果与 ANN 基准 |
| 50k–1M | HNSW + 精确重排 | 增量更新和高召回低延迟 |
| 超大/低内存 | 量化向量 + 磁盘分区 | 达到基准门后再引入 |

HNSW 必须保留精确旁路，因为研究证明图索引存在最坏情况退化。2026 年的新 ANN 算法先进入实验 provider，不直接替换稳定主线。

参考：[HNSW 原论文](https://arxiv.org/abs/1603.09320)、[ANN 最坏情况研究](https://proceedings.neurips.cc/paper_files/paper/2023/hash/d0ac28b79816b51124fcc804b2496a36-Abstract-Conference.html)。

## 3. 混合检索

搜索并行产生 FTS5 词法、结构化过滤、128 维语义向量和本地行为先验四路结果。无标注数据时用 RRF 合并名次；积累本地反馈后，以单参数凸组合校准词法/语义权重。不能直接相加 BM25 与余弦原始分数；研究表明少量域内样本下，学习后的凸组合通常优于固定 RRF。

参考：[混合检索融合分析](https://arxiv.org/abs/2210.11934)、[RRF 说明](https://learn.microsoft.com/en-us/azure/search/hybrid-search-ranking)。

## 4. 持久数据模型

schema 11 已落地：

```text
file_features
  file_id + feature_kind + model_version
  snapshot_size + modified_at_ns + changed_at_ns
  dimensions + quantization + feature_blob + created_at_ms

similarity_runs
  job_id + library_id + algorithm_version + thresholds_json
  benchmark_profile + completed_at_ms

similarity_edges
  run_id + left_file_id + right_file_id
  evidence_kind + raw_distance + calibrated_score + explanation_json
```

特征绑定稳定文件 ID、完整快照、模型版本和预处理版本；任一变化都使缓存失效。候选边只保存证据，不保存“应删除”结论。

## 5. Provider 边界

```text
FeatureProvider
  capabilities() -> media types + dimensions + model version
  estimate_cost(files) -> bytes/time/memory
  extract(file_id, authorized path, snapshot, cancellation) -> feature

CandidateIndex
  upsert/remove/rebuild
  query(feature, k, threshold)
  exact_baseline_query(feature, k)
```

provider 不接收 UI 传入的任意路径，只接收核心重新授权后的文件。模型下载、版本、SHA-256、许可证和占用空间必须在设置页展示。

## 6. 平台方案

- macOS：Vision Feature Print 做图片增强；Core Spotlight 只发布用户授权资料库的可搜索元数据。其索引保持本机私有，应用负责更新。
- Windows：优先查询 Windows Search 属性/内容索引，缺失格式再使用内部解析器，并遵循系统 CPU、I/O、电池、内存和磁盘节流。
- SQLite 始终是跨平台一致性事实来源；系统索引只是只读 provider。

参考：[Core Spotlight](https://developer.apple.com/documentation/CoreSpotlight)、[Windows Search](https://learn.microsoft.com/en-us/windows/win32/search/-search-indexing-process-overview)。

## 7. 验收门

- 数据集分为真重复、近重复、相似但不可合并三类。
- 报告 Recall@10、Precision@10、PR-AUC、危险误报率、冷/热吞吐、峰值 RSS、索引字节/文件和 p50/p95 延迟。
- 覆盖 10k/100k/1M 规模、读取中变化、暂停/取消、模型升级、索引损坏恢复。
- 精确重复危险误报必须为 0；近重复只读候选 Precision@10 ≥0.90；ANN Recall@10 相对精确基线 ≥0.95。
- 未达到门槛时只保留实验开关，不开放批量清理。

## 8. 实施顺序

1. schema 11：特征、运行记录和候选边。**已完成。**
2. 文本 SimHash、图片 dHash、快照缓存、持久可控任务与 Web/App 统一只读候选 UI。**已完成基础版；EXIF 方向归一化、MinHash/pHash 二次验证待接入。**
3. 精确搜索基准和阈值数据集。**下一检查点。**
4. macOS Vision provider；真机基准通过后启用。
5. 可选 EmbeddingGemma 128D 与 MobileCLIP2 provider，模型包独立下载校验。
6. 50k 向量以上启用 HNSW，并以精确 top-k 持续监测召回。
7. FTS5 + 向量混合搜索：先 RRF，后本地标注校准凸组合。
8. Core Spotlight / Windows Search provider 和平台发布验收。
