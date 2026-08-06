// Q2 故障注入：进程级强杀矩阵的受控执行体。
//
// execute 模式：建库 → 注册资料库 → 索引 fixture 文件 → 构造移动计划并执行；
//   `GUIXU_FAULT_POINT` 与 `fault-injection` feature 配合，在持久化节点循环等待 kill。
// recover 模式：重新打开同库，执行启动审计（audit_incomplete_operations），打印收敛结论。
//
// 用法（由 test-fixtures/kill-matrix.mjs 编排）：
//   GUIXU_FAULT_POINT=after_publish cargo run -p guixu-operations --features fault-injection \
//     --example kill_injection -- <db> <root> execute
//   cargo run -p guixu-operations --example kill_injection -- <db> <root> recover

use guixu_domain::{ConflictPolicy, FileOperationKind};
use guixu_operations::{execute_stored_plan, plan_renames};
use guixu_platform::observe_file;
use guixu_storage::{Database, NewPlan, NewPlanItem};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("用法: kill_injection <db> <root> <execute|recover>");
        std::process::exit(2);
    }
    let db_path = &args[1];
    // macOS /var 是 /private/var 的符号链接；统一 canonical 基准避免 OutsideRoot。
    let root = std::path::Path::new(&args[2])
        .canonicalize()
        .expect("canonicalize 根目录");
    let root = root.as_path();
    let mode = &args[3];
    let now = 1_000_000_i64;

    match mode.as_str() {
        "execute" => {
            let mut database = Database::open(db_path).expect("打开数据库");
            database
                .register_library_root(
                    "kill-library",
                    "kill-root",
                    "Kill Fixture",
                    root.to_str().unwrap(),
                    now,
                )
                .expect("注册资料库");
            // fixture：根目录下 source.txt 由编排脚本预置。
            let source = root.join("source.txt");
            let observed = observe_file(&source).expect("观察 fixture 文件");
            database
                .reconcile_file(
                    "kill-library",
                    "kill-file",
                    source.to_str().unwrap(),
                    &observed.identity,
                    &observed.snapshot,
                    now,
                )
                .expect("索引 fixture");
            let _target = root.join("moved.txt");
            let planned = plan_renames(
                root,
                &[guixu_operations::RenameRequest {
                    file_id: "kill-file".to_owned(),
                    source: source.clone(),
                    new_name: "moved.txt".to_owned(),
                    expected_identity: observed.identity.clone(),
                    expected_snapshot: observed.snapshot.clone(),
                }],
            )
            .expect("构造计划");
            database
                .create_plan(NewPlan {
                    id: "kill-plan",
                    library_id: "kill-library",
                    operation_kind: FileOperationKind::Rename,
                    conflict_policy: ConflictPolicy::Abort,
                    created_at_ms: now,
                    expires_at_ms: now + 60_000,
                    items: &planned
                        .iter()
                        .enumerate()
                        .map(|(index, item)| NewPlanItem {
                            ordinal: index as i64,
                            file_id: item.file_id.clone(),
                            source_path: item.source.to_string_lossy().into_owned(),
                            target_path: item.target.to_string_lossy().into_owned(),
                            expected_identity: item.expected_identity.clone(),
                            expected_snapshot: item.expected_snapshot.clone(),
                        })
                        .collect::<Vec<_>>(),
                })
                .expect("创建计划");
            execute_stored_plan(&mut database, root, "kill-plan", "kill-operation", now)
                .expect("执行计划");
            eprintln!("KILL_EXECUTE completed without fault");
        }
        "recover" => {
            let mut database = Database::open(db_path).expect("打开数据库");
            let audit = guixu_operations::audit_incomplete_operations(&mut database, now)
                .expect("启动审计");
            let operation = database
                .operation("kill-operation")
                .expect("查询操作")
                .expect("操作应存在");
            let source_exists = root.join("source.txt").exists();
            let target_exists = root.join("moved.txt").exists();
            println!(
                "KILL_RECOVER status={} source_exists={} target_exists={} \
                 audit={{completed:{}, not_started:{}, needs_attention:{}}}",
                operation.status,
                source_exists,
                target_exists,
                audit.completed,
                audit.not_started,
                audit.needs_attention
            );
        }
        _ => {
            eprintln!("未知模式: {mode}");
            std::process::exit(2);
        }
    }
}
