use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use guixu_analysis::{SimilarityHashInput, find_similarity_candidates};

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e3779b97f4a7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
    value ^ (value >> 31)
}

fn synthetic_hashes(count: usize) -> Vec<SimilarityHashInput> {
    (0..count)
        .map(|index| SimilarityHashInput {
            id: format!("file-{index}"),
            hash: splitmix64(index as u64),
        })
        .collect()
}

fn benchmark_similarity_index(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("hamming_multi_index");
    group.sample_size(10);
    for count in [10_000_usize, 100_000] {
        let inputs = synthetic_hashes(count);
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(count),
            &inputs,
            |bencher, inputs| {
                bencher.iter(|| find_similarity_candidates(inputs, 8).expect("valid radius"));
            },
        );
    }
    group.finish();
}

criterion_group!(benches, benchmark_similarity_index);
criterion_main!(benches);
