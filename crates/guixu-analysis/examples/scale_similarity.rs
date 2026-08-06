use std::env;
use std::error::Error;
use std::time::Instant;

use guixu_analysis::{SimilarityHashInput, find_similarity_candidates};

fn main() -> Result<(), Box<dyn Error>> {
    let count = env::args()
        .nth(1)
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(1_000_000);
    if !(1..=1_000_000).contains(&count) {
        return Err("count 必须在 1..=1000000 范围内".into());
    }
    let inputs = (0..count)
        .map(|index| SimilarityHashInput {
            id: index.to_string(),
            hash: splitmix64(index as u64),
        })
        .collect::<Vec<_>>();
    let started = Instant::now();
    let candidates = find_similarity_candidates(&inputs, 8)?;
    let elapsed = started.elapsed();
    println!(
        "{{\"inputs\":{count},\"candidate_pairs\":{},\"elapsed_ms\":{},\"elements_per_second\":{:.2}}}",
        candidates.len(),
        elapsed.as_millis(),
        count as f64 / elapsed.as_secs_f64(),
    );
    Ok(())
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e3779b97f4a7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
    value ^ (value >> 31)
}
