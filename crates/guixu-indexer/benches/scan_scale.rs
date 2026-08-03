// Q4 扫描扩展性基准：对 `test-fixtures/generate-scale.mjs` 生成的目录做全量扫描。
// 用法：
//   node test-fixtures/generate-scale.mjs e2e/.artifacts/scale 10000
//   GUIXU_SCALE_DIR=e2e/.artifacts/scale cargo bench -p guixu-indexer -- scan_scale
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use guixu_indexer::{ScanOptions, reconcile_snapshot};
use guixu_storage::Database;

fn scan_scale(criterion: &mut Criterion) {
    let directory = std::env::var("GUIXU_SCALE_DIR").unwrap_or_else(|_| {
        panic!("设置 GUIXU_SCALE_DIR 指向生成器输出目录（test-fixtures/generate-scale.mjs）")
    });
    let mut group = criterion.benchmark_group("scan");
    group.sample_size(10);
    group.bench_function("full-scan", |bencher| {
        bencher.iter_batched(
            || {
                let db_path = std::env::temp_dir()
                    .join(format!("guixu-scan-bench-{}.sqlite3", std::process::id()));
                let _ = std::fs::remove_file(&db_path);
                (
                    Database::open(&db_path).expect("open bench database"),
                    db_path,
                )
            },
            |(mut database, db_path)| {
                let report = reconcile_snapshot(
                    &mut database,
                    "bench",
                    std::path::Path::new(&directory),
                    1,
                    &ScanOptions::default(),
                    |_| true,
                )
                .expect("scan scale directory");
                std::hint::black_box(report.progress.indexed_files);
                let _ = std::fs::remove_file(&db_path);
            },
            BatchSize::SmallInput,
        )
    });
    group.finish();
}

criterion_group!(benches, scan_scale);
criterion_main!(benches);
