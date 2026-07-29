# 归序缺陷修复总体方案（2026-07 审计）

> 状态：实施跟踪稿 v2（2026-07-29）
>
> 依据：2026-07-29 全库审计、源码回读、SQLite 数据层实测，以及代码/Web/原生窗口交叉验证；当前 81 个 Rust 测试全绿
>
> 关联文档：[MASTER_PLAN](./MASTER_PLAN.md)、[ALGORITHM_DESIGN_2026](./ALGORITHM_DESIGN_2026.md)、[UI_POLISH_PLAN](./UI_POLISH_PLAN.md)、[DEVELOPMENT_STATUS](./DEVELOPMENT_STATUS.md)
>
> 行号基准：2026-07-29 未提交工作区版本；修复以函数名为准，行号仅供定位。

## 实施状态

| 批次 | 当前状态 | 已落地的关键结果 | 后续验收 |
| --- | --- | --- | --- |
| P0 数据安全 | 已完成 | 不完整扫描不再误标缺失；复制撤销逐项复检；跨卷移动按原文件 ID 更新身份并清理缓存 | 继续扩大真实外置卷组合 |
| P1 状态机 | 核心完成 | 条件状态转移、启动消费 queued、活动任务去重、结果与完成状态同事务、worker 打开失败收敛到 failed | RecoveryNeeded 的高级人工恢复 UI 继续增强 |
| P2 性能 | 代码完成 | FTS 更新触发器 guard；重复/相似分析使用 worker 连接与缓存 | 10 万文件与冷图片 p95 基准仍待建立 |
| P3 前端竞态 | 已完成 | 列表/搜索请求令牌、搜索态刷新保留上下文、计划创建互斥、通知优先级与键盘边界修复 | 持续真机回归 |
| P4 搜索与身份 | 基础完成 | Unicode/Trigram、结构化查询、generation 绑定、稳定游标与相似阈值边界 | 卷 UUID 与更多平台 provider |
| P5 低危清扫 | 持续进行 | 空特征不进入 SimHash、汉明距离越界显式报错、EXIF 方向归一化、UI/动效/窄窗口修复 | 随平台发布验收清扫 |

本轮验证基线为 81 项 Rust 测试、前端契约、严格 Clippy、JS 语法、diff whitespace、官方 Tauri debug `.app` 构建，以及 1180×760 / 900×620 真实窗口点击验证。逐模块证据与未完成门禁见 [交叉验证矩阵](./CROSS_VALIDATION_2026.md)。附录保留原始审计清单作为可追溯记录，不代表每项仍处于未修复状态。

---

## 1. 审计结论摘要

- **总计 68 项缺陷：高 8 / 中 21 / 低 39**（附录 A 为完整清单）。
- 算法核心（SimHash、dHash、9 段 LSH、RRF/凸融合、三级精确去重）数学实现正确；缺陷集中在**跨层集成**：索引对账、任务状态机、跨卷操作、FTS 同步、前端异步竞态。
- 最严重的是三类**数据安全**问题：撤销删除用户新内容（H2）、跨卷移动写坏索引（H3）、扫描跳过子树被误判为已删除（H1）。
- 现有测试全部通过——下述缺陷均落在测试盲区，每批次必须附带新测试（见第 10 节）。

### 严重度定义

| 级别 | 定义 |
| --- | --- |
| 高 | 可导致用户数据丢失/损坏、索引大面积错误、UI 长时间冻结或任务永久卡死 |
| 中 | 功能结果错误但有自愈/兜底路径，或特定平台/规模下必然失败 |
| 低 | 边界行为不正确、进度/提示误导、契约不一致，无数据风险 |

---

## 2. 修复批次总览

| 批次 | 内容 | 覆盖缺陷 | 预估 | 验收门 |
| --- | --- | --- | --- | --- |
| **P0** | 数据安全 | H1、H2、H3 + 关联低危 | 3–4 人日 | 新增 fixture 全绿 + 实机三场景 |
| **P1** | 状态机与任务生命周期 | H6 + 中危 9 项 | 4–5 人日 | 状态机单测 + 崩溃恢复实机 |
| **P2** | 性能 | H4、H5 | 2 人日 | 10 万文件基准达标 |
| **P3** | 前端竞态 | H7、H8 + 前端中低危 | 2–3 人日 | 竞态场景手测 + 控制台零错误 |
| **P4** | 搜索与平台身份 | 中危 6 项 + analysis 契约 | 3–4 人日 | 中文搜索用例 + 外置盘实机 |
| **P5** | 低危清扫 | 剩余全部低危 | 2–3 人日 | 全量回归 |
| **UI 轨道** | 排版/令牌/动效 | 见 [UI_POLISH_PLAN](./UI_POLISH_PLAN.md) | 独立排期 | 设计系统验收门 §5 |

依赖关系：P0 独立；P1 的状态机守卫依赖 P0 的 reconcile 变更先行合并；P2 依赖 P1（任务状态语义稳定后再动任务执行路径）；P3 与 P0–P2 可并行；P4 的身份变更依赖 P0（reconcile 支持身份更新）；P5 最后。UI 轨道全程可并行，仅"骨架屏/列表淡入"建议等 P3 请求令牌落地后实施。

---

## 3. P0 数据安全批次

### 3.1 H2 复制撤销删除已修改副本（最高优先）

**缺陷**：`crates/guixu-operations/src/lib.rs:756-830`。复制撤销先对全部副本做身份+快照+完整 BLAKE3 验证，再逐个 `fs::remove_file`；验证窗口随文件数线性放大（500 副本全量哈希可达数分钟），窗口内被用户修改/替换的副本被直接删除，历史记"回滚成功"。移动撤销每项执行前有复检，唯独复制没有。

**方案**：

1. 删除循环内、每项 `remove_file` 之前重新执行轻量复检：`canonicalize` + `contained` + 身份比对 + 快照（size/mtime/ctime）比对（stat 级，不重哈希）；任一不匹配 → 该项跳过并记 `verification_changed`，**整个撤销在该项前停止**，已完成项保持 RolledBack，操作置 RecoveryNeeded（P1 提供出口）。
2. 目标已不存在（`NotFound`）视为"该条目已回滚"：记 RolledBack 并继续，不再以 `canonicalize` 原始错误中止整个撤销（同时修复低危 L-ops-3）。
3. 全量哈希验证保留在第一阶段（不变）；文档化残留窗口：复检 stat 与 unlink 之间的固有竞态只能靠"先 rename 到隔离名再删"消除，作为后续可选加固，不在本批。

**测试**：注入验证后修改副本的 hook → 断言整体拒绝且已修改文件仍在；删除一个副本 → 其余副本正常回滚；500 副本批量撤销全成功路径。

### 3.2 H3 跨卷移动写坏索引

**缺陷**：`crates/guixu-operations/src/lib.rs:592-599` 在跨卷移动（EXDEV 回退是设计内路径）物理完成后，调用 `reconcile_file` 传入**原 file_id + 新身份**；`crates/guixu-storage/src/lib.rs:1907-1929` 按身份查找落空 → 走 `:1973-1991` INSERT 分支 → `files.id` 主键冲突，错误在条目已记 Completed 后抛出：文件已搬走、操作卡 executing、索引仍指向已消失的源路径且未标 missing。撤销恢复方向同坏（`:710-717` 合成常量 id `restored-file`，多条目二次插入即冲突）。

**方案**：

1. `reconcile_file_on` 增加第三分支：身份查找落空时按 `new_file_id` 查找；命中且身份不同 → **UPDATE 该行的身份列**（platform/volume_id/native_file_id/generation）+ current_path + 快照 + 路径历史，返回 `Moved`；同时清除该行 `file_hash_cache` 与 `file_features`（身份变化后旧缓存不可信；ctime 变化本会使缓存读时失效，但显式清除避免残留）。
2. 撤销恢复路径把原 `file_id` 随计划条目传递，禁止多条目共用合成常量 id；`item.file_id` 缺失时的兜底 id 必须含 ordinal（与 copy 分支 `copy-{operation_id}-{ordinal}` 同构）。
3. 身份 UPDATE 与 `0001_initial.sql` 的 UNIQUE(platform,volume_id,native_file_id,generation) 交互：目标身份已被其他记录占用时返回显式冲突错误并进入 RecoveryNeeded，不静默覆盖（inode 复用场景，与 P4 的 generation 加固联动）。

**测试**：storage 级单测直接以"同 id、新身份"调用 `reconcile_file` → 断言 UPDATE 而非 PK 错误、路径历史正确闭合、缓存被清；目标身份被占用 → 显式冲突。集成层跨卷用 mount fixture 不可移植，以 storage 级覆盖为准。

### 3.3 H1 扫描跳过子树被误判为已删除

**缺陷**：`crates/guixu-indexer/src/lib.rs:359-364` 的 `reconcile_snapshot` 只以 `!report.cancelled` 作为 `mark_unseen_missing` 的前置条件；`scan_library` 对不可读子目录（`:269-280`）、entry 读取错误（`:227-235`）、非 UTF-8 路径（`:287-296`）记一条 issue 就跳过，被跳过子树的 `last_seen_at_ms` 不更新，随后被 `mark_unseen_missing` 全部置 missing。注释声称"只有完整扫描未取消时才标记"，但带 issue 的扫描并不完整。

**方案**：

1. 前置条件改为 `!report.cancelled && report.issues.is_empty()`——有跳过即抑制缺失标记，与取消同级处理。取舍：**真实删除会延迟到下一次无 issue 的干净扫描才被标记**，这是正确的保守方向（误删状态远比晚标记危险）；`max_reported_issues` 会截断 issue 列表，因此不能用"豁免被跳过子树"的精细方案（截断后无法枚举全部跳过点），全局抑制是唯一可靠实现。
2. `ScanReport` 增加 `incomplete: bool`（issues 非空即真），任务结果与前端通知区分"索引完成"与"索引完成（N 处跳过，未更新删除状态）"，避免用户误以为删除检测已生效。
3. 关联修复：`scan_library` 迭代中途出错的 issue 路径误记为扫描根（`:232` 栈帧不存路径）→ 栈帧补存目录路径，issue 归因到出错目录。

**测试**：fixture 建 chmod 000 子目录（含已索引文件）→ 重扫 → 断言文件未标 missing 且报告 `incomplete=true`；恢复权限 → 重扫 → 正常标记。

---

## 4. P1 状态机与任务生命周期批次

### 4.1 H6 任务完成/取消竞态

**缺陷**：`apps/desktop/src-tauri/src/lib.rs:2212-2234` 分析任务 `interrupted==false` 分支直接 `save_job_result + complete_job`，完成前不重查状态；`complete_job` 的条件 UPDATE（`guixu-storage/src/lib.rs:1049` 起，要求 `status='running'`）在用户已请求取消/暂停时影响 0 行，被 `let _ =` 吞掉 → 任务卡 `cancel_requested` 至 24h 租约过期，结果已保存但不可见。扫描路径的重查（`:2099`）与 `complete_job`（`:2110`）也是两个独立事务，有毫秒级窗口；`fail_job`（`:2076`）同型。

**方案**：

1. storage 层 `complete_job`/`fail_job`/`acknowledge_job_control` 返回值改为转移结果枚举（`Applied` / `Rejected{current_status}`），不再静默吞 0 行。
2. tauri 层完成序列改为单事务条件转移："running→completed" 或 "cancel_requested→cancelled（附结果）" / "pause_requested→paused（附部分结果）"，由一条带状态 CASE 的 UPDATE 完成，消除两事务窗口；结果 JSON 保存与状态转移同事务。
3. `recover_expired_jobs` 的三类强制跃迁补写 `job_events`（同时修复低危 L-sto-9）。

**测试**：storage 状态机单测：running→cancel_requested→complete_job → 断言最终 cancelled 且结果保留；每种非法跃迁断言拒绝。

### 4.2 启动恢复与入队去重

**缺陷**：`tauri:2499-2524` setup 不消费崩溃遗留的 queued 任务（入队即 spawn worker 是唯一消费触发），running 任务挂 24h；`tauri:417-451` 选文件夹先注册+入队再启动 watcher，watcher 失败留下永远 queued 的扫描；`tauri:460-490,1629-1721,2390-2450` 任务入队零去重（连点"重新扫描"并发多份全库哈希；每个脏目录各 spawn 一个 worker 线程互等 WAL 写锁）。

**方案**：

1. setup 序列：先 `recover_expired_jobs`，再扫描 queued 任务并统一 spawn 消费循环。
2. 入队前去重：同 library 同 kind 存在 running/queued → 直接返回已有任务 id（前端表现为复用任务）。
3. 目录对账改为每 library 单 worker + 脏目录队列合并（窗口期内合并重复目录），不再每目录一线程；worker 连接复用，migrate 版本检查只在打开时一次。
4. `select_library_folder` 顺序调整：watcher 启动成功后再注册+入队；失败时无残留。

### 4.3 计划/操作状态机守卫

**缺陷**：`guixu-storage/src/lib.rs:559-661` 状态列无 CHECK、更新无当前状态条件（Completed→Executing 可静默入库；对不存在的 id 也静默 Ok）；`log_operation_intent`（`:567-610`）不校验计划 status/expires_at_ms，同一计划可生成多个操作重复执行；`operations:543` 计划置 executing 后失败路径不复位，永远不能重试。

**方案**：

1. Rust 层定义 plans/operations/jobs 三张转移表（唯一事实来源），`update_*_status` 生成带 `WHERE status IN (allowed…)` 的条件 UPDATE 并校验影响行数，非法跃迁返回 `IllegalTransition{from,to}`。
2. 迁移 0012 为 status 列补 CHECK（ STRICT 表 ALTER 受限，评估表重建成本；若成本高则本批只做 Rust 层守卫，CHECK 列入后续）。
3. `log_operation_intent` 增加 `status='ready' AND expires_at_ms>now` 条件与"同计划已存在活动操作"唯一性检查。
4. 计划执行失败/中断路径复位：`executing → ready`（可重试）或 `expired`（超时），不留 executing 死状态。

### 4.4 RecoveryNeeded 出口

**缺陷**：`operations:654,969-980,1099-1120` RecoveryNeeded 无 resume、无重试、撤销只接受 completed；为跨卷部分失败设计的 `classify_move_recovery` 生产代码零调用。

**方案**：

1. 撤销入口接受 `completed` 与 `recovery_needed`：对 recovery_needed 只回滚状态为 Completed/Published 的条目，失败/未执行条目跳过并在报告中列明。
2. 新增"重试失败条目"命令：对 recovery_needed 操作重建仅含失败条目的新计划（复用现有计划状态机与 preflight）。
3. 启动审计接入 `classify_move_recovery`：可判定的部分失败自动收敛（RemovePublishedSource 等），不可判定的保持 RecoveryNeeded 并在 UI 给出两个显式动作（重试失败项 / 回滚已完成项），不再有死胡同。

### 4.5 撤销三层验证推广

**缺陷**：`operations:660-680,1123-1132` 移动/重命名/废纸篓/整理的撤销只验身份一层；`record_published_operation_item`（`:574-581`）对所有操作类型都存了 target_snapshot 与 content_hash 但非复制撤销从不读取。叠加平台层 generation 恒 None，inode 复用时撤销会把无关文件搬到旧路径报"回滚成功"。

**方案**：非复制撤销统一走 `validate_published_target`（身份+快照+内容哈希），与复制撤销同一契约；哈希成本随撤销线性，符合"撤销必须整体拒绝"的安全语义。与 P4 的 st_gen 加固叠加后，身份层可信度高、哈希开销可按操作类型分级（移动/重命名：身份+快照+哈希；废纸篓恢复：身份+快照）。

### 4.6 命令层阻塞与杂项中危

1. 四个计划创建命令（`tauri:820-1225`）与 `preview_file` 改 `async` + `spawn_blocking`，消除主线程文件 I/O。
2. `execute_stored_plan`/undo 增加进度上报与协作式取消点（每条目检查），双击并发执行由 4.3 的状态守卫拦截。
3. `operations:1024-1028` 跨卷回退映射补 ENOTSUP/EPERM（exFAT/SMB 不支持 RENAME_EXCL）。
4. `operations:376` 仅大小写/规范化形式不同的重命名：目标存在但解析身份与源相同 → 允许，经临时名两步 rename 执行（大小写不敏感 APFS）。
5. `operations:177-212` `plan_trash` 补重复目标检测（对齐 plan_copies/plan_renames）。

---

## 5. P2 性能批次

### 5.1 H4 FTS 触发器 O(n²)

**缺陷**：`migrations/0002:18-22`、`0004:17-21` 的同步触发器对 UNINDEXED 的 `file_id` 列 `DELETE`（全 FTS 表扫描），且 `reconcile_file_on` 的 UPDATE 无条件 SET `current_path`（`storage:1946-1948`）→ 无变化重扫每文件触发 2 张 FTS 表全表扫描+重插。

**方案**（迁移 0012）：

1. 两张 FTS 表的 update 触发器加 `WHEN old.current_path <> new.current_path OR old.library_id <> new.library_id`（SQLite 的 AFTER UPDATE OF 按 SET 列表触发，WHEN 子句是有效拦截层）。
2. `reconcile_file_on` 的 UPDATE 拆分：`moved` 时才 SET `current_path`，未移动只更新快照列（双保险）。
3. 基准验收：10 万文件无变化重扫，扫描阶段耗时应回到线性（目标 < 2 倍 1 万文件 × 10）。

### 5.2 H5 重复分析主锁冻结 UI

**缺陷**：`tauri:1740-1818` `compute_exact_duplicates` 持主数据库互斥锁完成全量列举（逐文件 canonicalize + 缓存查询）与哈希写回（逐行 autocommit + synchronous=FULL = 每行 fsync）；相似分析却用独立 worker 连接。

**方案**：

1. 改用 `open_worker_database` 独立连接（与 similar_texts/images 对齐）。
2. storage 新增 `save_file_hash_cache_batch`：单事务批量写回，消除每行 fsync。
3. 列举分页间释放连接（页大小 500 已合适）。
4. 验收：10 万文件首次重复分析期间，jobs 轮询 p95 延迟 < 500ms，列表/搜索可用。

---

## 6. P3 前端竞态批次

### 6.1 H7 扫描完成摧毁搜索态

**缺陷**：`app.js:1507-1510` 扫描/对账完成 → 无条件 `loadFiles(true)`：清空搜索结果（查询词残留）、清空多选、关闭预览。

**方案**：

1. 完成刷新改为"原位刷新"：搜索态（搜索框非空或智能文件夹激活）→ 以当前查询重新执行搜索并保留仍存在的选中项与预览；浏览态 → `loadFiles(true)` 同样保留选中交集。
2. `snapshot_reconcile` 完成不触发整表刷新（其频次高），仅 `library_scan` 完成触发；后端在任务结果中附带 `changed_files` 计数，0 变化跳过刷新。

### 6.2 H8 列表/搜索竞态

**缺陷**：`app.js:512-568` `loadFiles` 与 `runSearch` 无互斥无请求令牌：搜索途中"加载更多"响应到达会把未过滤分页追加进搜索结果；连续快速搜索旧响应覆盖新结果。

**方案**：引入单调 `listRequestId`（对齐现有 `previewRequestId` 模式）：loadFiles/runSearch/smartFolder 共用一个计数器，响应到达时比对令牌，过期即丢弃；`state.loading` 改为对两个入口同时生效。

### 6.3 执行流与弹窗中危

1. `executeCurrentPlan`/`confirmUndoOperation`（`app.js:903-920,1000-1014`）拆分 try：执行成功与后续 rescan/refresh 分离，后者失败只提示"操作已成功，同步稍后重试"，不再误报失败。
2. 计划创建串行化：创建期间禁用全部批量按钮 + `showModal()` 前 `.open` 守卫 + 创建令牌防覆盖（`app.js:862` 等）。
3. 撤销失败路径：不关弹窗时保留 `state.pendingUndo`，"验证并撤销"可重试（修复 `app.js:994-1018` 死按钮）。
4. `refreshJobs` 的 busy 通知改写只在任务真的完成过时触发，不抹掉执行中警告（`app.js:1511-1513`）。

---

## 7. P4 搜索与平台身份批次

### 7.1 中文搜索与排名

1. **bm25 权重修正**（`storage:1502`）：`bm25(files_fts, 0.0, 0.0, 10.0, 2.0)`——权重按列声明序绑定，前两列是 UNINDEXED 的 file_id/library_id，当前 10.0/2.0 全部落空。
2. **1–2 字中文搜索**（`storage:1458-1462`）：全部词元 ≥3 字符走 trigram 不变；含 1–2 字符词元时，文本条件改走 `LIKE '%词%'` 子串匹配（转义 `%`/`_`），排名置于 FTS 结果之后；单字查询规模大可加 LIMIT 保护。验收用例：`目计`、`划` 必须命中 `项目计划.md`。
3. 查询词元截断（`storage:1786,1796,1808` 前 24/12 个）改为显式拒绝并在 UI 提示，不再静默丢条件。

### 7.2 平台身份加固

1. **generation**：macOS 经 `libc::stat` 取 `st_gen` 填入 `FileIdentity.generation`（std 不暴露，需直接 libc 调用；Linux 保持 None 并在文档注明 inode 复用风险由快照层兜底）。
2. **volume_id**：macOS 经 `getattrlist(ATTR_VOL_UUID)` 取卷 UUID，失败回退 st_dev 并记录降级；存量数据迁移：0012 不强制改写旧身份，首次重扫时按 P0-3.2 的身份 UPDATE 分支自然收敛（同 id 新身份 → 更新），避免全量重插。
3. **硬链接去重**（`indexer:297-303`）：扫描批次内按身份去重，同身份只保留首个路径并计数 `hardlink_aliases`；alias 表持久化列入后续（不阻塞本批）。
4. 身份语义变更写入 `docs/ALGORITHM_DESIGN_2026.md` 第 1 节安全层描述。

### 7.3 analysis 契约修复（中危 2 项）

1. `text_simhash` 对过滤后为空/<3 字符的输入改为返回 `Option<u64>`（None=无有效特征），调用方显式跳过；消除"空文本坍缩为同一常量"的库级陷阱（当前调用方 24 字符防护使其实际不可达，但契约必须正确）。
2. `find_exact_duplicates_cached_controlled`：分桶遍历按键排序、`computed_hashes` 输出按路径排序，恢复 crate 顶部"确定性"契约；输入路径去重（同路径重复输入不再生成 [p,p] 假组）。
3. `find_similarity_candidates` 的 `maximum_distance.min(8)` 改为：>8 时返回显式错误或在文档注释中声明截断语义（二选一，倾向显式错误）。
4. 进度 `total_work` 只计入大小桶 ≥2 的文件（修复进度条到不了 100%）。

---

## 8. P5 低危清扫批次

按模块归并一次性清扫（完整清单见附录 A 低危区），执行顺序建议：storage 提示类 → tauri 杂项 → indexer 杂项 → analysis 边界 → 前端杂项。代表性项：

- storage：size u64→i64 溢出误报 InvalidProgress 改专属错误；`latest_library_root` 排序补 id tiebreaker；queued 任务暂停/取消事件与状态一致化；`update_job_progress` 校验缺口。
- tauri：`files_page` limit 钳上限；`validate_authorized_path` 与 open 之间的 TOCTOU 用 `O_NOFOLLOW` 打开收紧；watcher 只增不减（切库移除旧 watcher）；崩溃残留 `.guixu-*.tmp` 启动清扫；移除或接入三个未被前端调用的直接分析命令；`RuntimeInfo.version` 取应用版本而非 domain crate 版本。
- indexer：ReadDir 每层驻留 fd → 改栈帧存路径+进入时重开（消除 ~250 层 fd 上限）；脏目录已删除的 reconcile 按成功空处理；`mark_event_path` canonicalize 失败的回退路径规范化后再比对。
- analysis：文件名 255 字节总上限含扩展名；`safe_filename` 扩展名过滤控制字符；dotfile 理由文案改"名称以点开头"；SimHash 平局位改 `> 0` 对齐 Charikar；`classify_file` 的 .pages/.numbers zip 容器归 Document。
- operations：非 macOS `rename_exclusive` hard_link 成功 remove 失败归入 PublishedSourceRetained；目标悬空符号链接在规划期识别。
- 前端：ISO 日期改本地时区格式化；无选中时 ↓ 从第一项开始；视图切换按钮字形/aria 与状态同步；`applySettings` 同步 viewMode 按钮。

---

## 9. UI 与动效轨道

UI 整改已有独立方案 [UI_POLISH_PLAN](./UI_POLISH_PLAN.md)（四阶段：排版标尺 → 令牌落地 → 组件统一 → 动效系统），与本方案并行排期。两处衔接：

1. UI 轨道的"列表淡入/骨架屏/搜索状态保持"依赖 P3 的请求令牌（6.2），建议 UI 第 4 阶段（动效）在 P3 合并后启动；
2. 动效令牌落地后必须被现有"减少动态效果"设置（`app.css:257`、`app.js:1549`）自动管辖，并补 `prefers-reduced-motion` 媒体查询默认值。

验收门沿用 [UI_DESIGN_SYSTEM](./UI_DESIGN_SYSTEM.md) §5（1180×760 与 900×620 双窗口、键盘焦点、控制台零错误）。

---

## 10. 测试与验收计划

### 每批必带的新测试

| 批次 | 新测试/fixture |
| --- | --- |
| P0 | chmod 000 子目录重扫不标 missing；撤销窗口内修改副本整体拒绝；副本已被删其余照滚；reconcile 同 id 新身份 UPDATE；目标身份占用显式冲突 |
| P1 | 状态机转移表全非法跃迁拒绝；cancel_requested 后 complete 收敛 cancelled；启动恢复消费 queued；log_operation_intent 拒绝过期计划；ENOTSUP 回退；A.txt→a.txt 成功 |
| P2 | 10 万文件无变化重扫线性基准；分析期间 jobs 轮询 p95 < 500ms |
| P3 | 搜索中扫描完成保搜索态；过期响应被令牌丢弃（注入延迟） |
| P4 | `目计`/`划` 命中用例；bm25 name 优先用例；硬链接 fixture 无幽灵 Moved；delete+recreate 生成新记录（st_gen） |
| P5 | 0 字节精确重复成组；HD=8 边界召回 100%；sample_offsets 边界（65536/65537）；空文本 simhash None |

### 全局验收门

1. `cargo test --workspace` 全绿；新测试覆盖对应缺陷（缺陷复现 → 修复 → 转绿的顺序提交）。
2. 实机场景：外置盘资料库、含 chmod 000 目录的资料库、10 万文件库、扫描中取消/暂停、执行中强退重启。
3. 前端控制台零错误；UI 双窗口验收（如触及前端）。

---

## 11. 风险与回滚约定

- 迁移 0012 只新增/替换触发器与 CHECK（表重建若必要），不改写存量行；身份语义变更向前兼容（旧身份重扫自然收敛，见 7.2）。
- 每批次独立 commit/分支，按 P0→P5 顺序合入；P0 单独先行，不与其他批捆绑。
- 任何批次合入前跑全局验收门；性能批次不达标只回退该批。

---

## 12. 里程碑与工作量估算

| 里程碑 | 内容 | 预估 | 建议顺序 |
| --- | --- | --- | --- |
| R1 | P0 数据安全 | 3–4 人日 | 1 |
| R2 | P1 状态机 | 4–5 人日 | 2 |
| R3 | P2 性能 | 2 人日 | 3 |
| R4 | P3 前端竞态 | 2–3 人日 | 可与 R1–R3 并行 |
| R5 | P4 搜索与身份 | 3–4 人日 | 4 |
| R6 | P5 低危清扫 | 2–3 人日 | 5 |
| UI | 见 UI_POLISH_PLAN | 独立 | 并行；动效阶段在 R4 后 |

合计约 16–21 人日（不含 UI 轨道）。估算按"熟悉代码库的单个开发者 + 测试"口径，评审与联调另计。

---

## 附录 A：缺陷总表（68 项）

> H=高 M=中 L=低；行号基准见文首。

### A.1 guixu-analysis（9）

| # | 级别 | 位置 | 缺陷 | 批次 |
| --- | --- | --- | --- | --- |
| 1 | M | lib.rs:402-404 | 无字母数字文本坍缩为同一 SimHash 常量（当前调用方 24 字符防护，实际不可达） | P4 |
| 2 | M | lib.rs:207,257,330 | HashMap 迭代序致 computed_hashes 顺序与中断缓存子集跨运行不确定 | P4 |
| 3 | L | lib.rs:421 | SimHash 平局位 `>=0` 偏向 1，偏离 Charikar `>0` | P5 |
| 4 | L | lib.rs:503 | `maximum_distance.min(8)` 静默截断 | P4 |
| 5 | L | lib.rs:296-299,307-320 | 重复输入路径生成 [p,p] 假组 | P4 |
| 6 | L | lib.rs:204 | total_work 含单例文件，进度到不了 100% | P4 |
| 7 | L | lib.rs:681-690,713-716 | 文件名 220 字节上限不含扩展名/冲突后缀，可超 255 | P5 |
| 8 | L | lib.rs:132-137 | dotfile 被误报"位于隐藏目录"；`..` 组件误判 | P5 |
| 9 | L | lib.rs:719-960 | 测试缺口：HD=8 召回、0 字节成组、空文本、采样边界、确定性 | P5 |

### A.2 guixu-operations（11）

| # | 级别 | 位置 | 缺陷 | 批次 |
| --- | --- | --- | --- | --- |
| 1 | H | lib.rs:756-830 | 复制撤销验证与删除间 TOCTOU，删已修改副本 | P0 |
| 2 | H | lib.rs:592-599,710-717 | 跨卷移动/恢复 reconcile 主键冲突写坏索引 | P0 |
| 3 | M | lib.rs:1006-1077,612-616 | 发布后校验失败无补偿，"磁盘成功历史失败"逃出审计 | P1 |
| 4 | M | lib.rs:654,969-980,1099-1120 | RecoveryNeeded 死胡同；classify_move_recovery 零调用 | P1 |
| 5 | M | lib.rs:660-680,1123-1132 | 非复制撤销只验身份，不验快照+内容 | P1 |
| 6 | M | lib.rs:1024-1028 | ENOTSUP（exFAT/SMB）未映射 copy+delete 回退 | P1 |
| 7 | M | lib.rs:376 | 仅大小写不同的重命名被误拒 | P1 |
| 8 | L | lib.rs:876-897 | 中断撤销审计只 Trash 分支修索引 | P5 |
| 9 | L | lib.rs:177-212 | plan_trash 无重复目标检测 | P1 |
| 10 | L | lib.rs:758 | 副本被删后整个撤销无法推进 | P0 |
| 11 | L | lib.rs:1186-1229 等 | tmp 残留无清扫；hard_link 半失败未归类；悬空链接规划期漏检 | P5 |

### A.3 guixu-indexer / platform / domain（10）

| # | 级别 | 位置 | 缺陷 | 批次 |
| --- | --- | --- | --- | --- |
| 1 | H | indexer:359-364 | 带 issue 扫描触发误判 missing | P0 |
| 2 | M | indexer:297-303 | 硬链接无身份去重，幽灵 Moved/历史膨胀 | P4 |
| 3 | M | platform:117,140 | generation 恒 None，inode 复用继承旧记录 | P4 |
| 4 | M | platform:115,138 | st_dev 非持久卷标识，重挂载整卷身份断裂 | P4 |
| 5 | L | indexer:391-404 + storage:1946-1958 | 并发对账 last_seen 回写旧值放大误标 | P1 |
| 6 | L | indexer:232 | 迭代中途错误 issue 归因到根目录 | P0 |
| 7 | L | indexer:208,270 | DFS 每层驻留 fd，~250 层后子树跳过 | P5 |
| 8 | L | indexer:96-101 | mark_event_path canonicalize 失败静默丢事件 | P5 |
| 9 | L | indexer:350 | 脏目录已删除致整个 reconcile 任务失败 | P5 |
| 10 | L | domain/api.rs:19 | RuntimeInfo.version 取 domain crate 版本 | P5 |

### A.4 guixu-storage（16）

| # | 级别 | 位置 | 缺陷 | 批次 |
| --- | --- | --- | --- | --- |
| 1 | H | 0002:18-22,0004:17-21 + lib.rs:1946-1958 | FTS 触发器 UNINDEXED 删除 + 无条件 UPDATE → 重扫 O(n²) | P2 |
| 2 | M | lib.rs:559-661 | 状态机无守卫，非法跃迁静默入库 | P1 |
| 3 | M | lib.rs:1458-1467 | 1–2 字中文非词首搜索静默漏结果 | P4 |
| 4 | M | lib.rs:1502,1507 | bm25 权重绑到 UNINDEXED 列，文件名加权失效 | P4 |
| 5 | M | lib.rs:567-610 | log_operation_intent 不校验计划状态/过期 | P1 |
| 6 | L | 0001:32 + lib.rs:1906-1928 | 身份 UNIQUE 对 NULL generation 失效，可双记录 | P4 |
| 7 | L | lib.rs:1936-1945 | 同批同毫秒 A→B→A 移动 file_paths 主键冲突 | P1 |
| 8 | L | lib.rs:316-346 | register_library_root check-then-act 可重复注册 | P5 |
| 9 | L | lib.rs:1004-1024,2059-2076 | queued 任务控制/强制跃迁事件流不一致 | P1 |
| 10 | L | lib.rs:955-957 | update_job_progress 校验缺口，进度可超 100% | P5 |
| 11 | L | lib.rs:1786,1796,1808 | 查询词元静默截断（24/12） | P4 |
| 12 | L | lib.rs:354-371 | latest_library_root 同毫秒无 tiebreaker | P5 |
| 13 | L | 0001:65 vs 47 | 未来删库级联被 operations 阻断（潜伏） | P5 |
| 14 | L | lib.rs:1959-1961 | 元数据变化只清哈希缓存，特征行膨胀 | P5 |
| 15 | L | lib.rs:1157 等 | size 溢出误报 InvalidProgress | P5 |
| 16 | L | — | （确认项合并入上） | — |

### A.5 Tauri 命令层（12）

| # | 级别 | 位置 | 缺陷 | 批次 |
| --- | --- | --- | --- | --- |
| 1 | H | lib.rs:1740-1818 | 重复分析持主锁，UI 冻结 | P2 |
| 2 | H | lib.rs:2212-2234 | 完成/取消竞态，任务卡 24h | P1 |
| 3 | M | lib.rs:1252-1286,1887-1915 | 执行/撤销无进度无取消点；并发执行检查非原子 | P1 |
| 4 | M | lib.rs:460-490,1629-1721,2390-2450 | 任务入队零去重；每脏目录一 worker | P1 |
| 5 | M | lib.rs:2499-2524 | 启动不恢复任务队列 | P1 |
| 6 | M | lib.rs:820-1225,551-614 | 计划创建/预览在主线程阻塞 I/O | P1 |
| 7 | L | lib.rs:417-451 | watcher 失败留下永远 queued 的扫描 | P1 |
| 8 | L | lib.rs:2135-2138,2256-2259 | worker 打开失败静默 return，任务卡 running | P1 |
| 9 | L | lib.rs:444-448,2390-2450 | watcher 只增不减 | P5 |
| 10 | L | lib.rs:506 | files_page limit 无上限 | P5 |
| 11 | L | lib.rs:666-678 | 授权校验与 open 间 TOCTOU | P5 |
| 12 | L | lib.rs:1682-1692 等 | 三个直接分析命令为无取消死命令面 | P5 |

### A.6 前端 app.js（10）

| # | 级别 | 位置 | 缺陷 | 批次 |
| --- | --- | --- | --- | --- |
| 1 | H | app.js:1507-1510 | 扫描完成无条件 loadFiles(true) 摧毁搜索态/选择/预览 | P3 |
| 2 | H | app.js:512-568 | loadFiles/runSearch 无互斥，数据混杂 | P3 |
| 3 | M | app.js:545-568 | 连续搜索旧响应覆盖新结果 | P3 |
| 4 | M | app.js:903-920,1000-1014 | 成功后置步骤失败误报操作失败 | P3 |
| 5 | M | app.js:862,670-760 | 计划可并发创建，弹窗覆盖+假错误 | P3 |
| 6 | L | app.js:994-1018 | 撤销失败后"验证并撤销"死按钮 | P3 |
| 7 | L | app.js:1511-1513 | busy 通知被轮询提前抹掉 | P3 |
| 8 | L | app.js:1537,1716-1721 | applySettings 不同步视图按钮字形/aria | P5 |
| 9 | L | app.js:341 | ISO 日期用 UTC，UTC+8 凌晨文件差一天 | P5 |
| 10 | L | app.js:495 | 无选中时 ↓ 跳过第一项 | P5 |
