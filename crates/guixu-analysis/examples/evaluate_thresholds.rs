use std::env;
use std::error::Error;
use std::fs;
use std::path::Path;

use guixu_analysis::{
    LabeledSimilaritySample, evaluate_similarity_threshold,
    select_similarity_threshold_with_precision_floor,
};

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args().skip(1);
    let path = arguments.next().ok_or(
        "用法：cargo run -p guixu-analysis --example evaluate_thresholds -- <labels.csv> [minimum_precision]",
    )?;
    let minimum_precision = arguments
        .next()
        .map(|value| value.parse::<f32>())
        .transpose()?
        .unwrap_or(0.90);
    if arguments.next().is_some() {
        return Err("参数过多".into());
    }
    let (training, validation) = read_samples(Path::new(&path))?;
    if training.is_empty() || validation.is_empty() {
        return Err("CSV 必须同时包含 train 与 validation 样本".into());
    }
    let primary_distances = (0..=8).collect::<Vec<_>>();
    let secondary_similarities = (0..=100)
        .map(|value| value as f32 / 100.0)
        .collect::<Vec<_>>();
    let selected = select_similarity_threshold_with_precision_floor(
        &training,
        &primary_distances,
        &secondary_similarities,
        minimum_precision,
    )
    .ok_or("训练集没有门限满足最低 precision")?;
    let validation_metrics = evaluate_similarity_threshold(
        &validation,
        selected.maximum_primary_distance,
        selected.minimum_secondary_similarity,
    );
    println!(
        "{{\"training_samples\":{},\"validation_samples\":{},\"maximum_primary_distance\":{},\"minimum_secondary_similarity\":{:.2},\"minimum_precision_gate\":{:.2},\"validation\":{{\"tp\":{},\"fp\":{},\"tn\":{},\"fn\":{},\"precision\":{:.6},\"recall\":{:.6},\"f1\":{:.6},\"false_positive_rate\":{:.6},\"accuracy\":{:.6}}}}}",
        training.len(),
        validation.len(),
        selected.maximum_primary_distance,
        selected.minimum_secondary_similarity,
        minimum_precision,
        validation_metrics.true_positives,
        validation_metrics.false_positives,
        validation_metrics.true_negatives,
        validation_metrics.false_negatives,
        validation_metrics.precision,
        validation_metrics.recall,
        validation_metrics.f1,
        validation_metrics.false_positive_rate,
        validation_metrics.accuracy,
    );
    Ok(())
}

fn read_samples(
    path: &Path,
) -> Result<(Vec<LabeledSimilaritySample>, Vec<LabeledSimilaritySample>), Box<dyn Error>> {
    let contents = fs::read_to_string(path)?;
    let mut training = Vec::new();
    let mut validation = Vec::new();
    for (index, line) in contents.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("split,") {
            continue;
        }
        let fields = line.split(',').map(str::trim).collect::<Vec<_>>();
        if fields.len() != 4 {
            return Err(format!("第 {} 行必须有 4 列", index + 1).into());
        }
        let sample = LabeledSimilaritySample {
            primary_distance: fields[1].parse()?,
            secondary_similarity: fields[2].parse()?,
            is_match: match fields[3] {
                "1" | "true" => true,
                "0" | "false" => false,
                _ => {
                    return Err(
                        format!("第 {} 行 is_match 必须是 true/false/1/0", index + 1).into(),
                    );
                }
            },
        };
        match fields[0] {
            "train" => training.push(sample),
            "validation" => validation.push(sample),
            _ => {
                return Err(format!("第 {} 行 split 必须是 train 或 validation", index + 1).into());
            }
        }
    }
    Ok((training, validation))
}
