//! 可验证、确定性且完全本地的文件分析算法。

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use guixu_domain::FileSnapshot;
use guixu_platform::observe_file;
use image::imageops::FilterType;
use image::{DynamicImage, ImageDecoder, ImageReader, Limits};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

const SAMPLE_BYTES: usize = 64 * 1024;
pub const EXACT_HASH_ALGORITHM: &str = "blake3-sampled-full-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateGroup {
    pub content_hash: [u8; 32],
    pub size: u64,
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DuplicateStats {
    pub input_files: usize,
    pub quick_fingerprinted_files: usize,
    pub fully_hashed_files: usize,
    pub quick_cache_hits: usize,
    pub full_cache_hits: usize,
    pub skipped_files: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateReport {
    pub groups: Vec<DuplicateGroup>,
    pub stats: DuplicateStats,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateInput {
    pub path: PathBuf,
    pub snapshot: FileSnapshot,
    pub cached_quick_fingerprint: Option<[u8; 32]>,
    pub cached_content_hash: Option<[u8; 32]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputedFileHash {
    pub path: PathBuf,
    pub quick_fingerprint: [u8; 32],
    pub content_hash: Option<[u8; 32]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedDuplicateReport {
    pub report: DuplicateReport,
    pub computed_hashes: Vec<ComputedFileHash>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuplicatePhase {
    QuickFingerprint,
    FullHash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DuplicateProgress {
    pub phase: DuplicatePhase,
    pub processed_files: usize,
    pub total_work: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlledDuplicateReport {
    pub result: CachedDuplicateReport,
    pub interrupted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionInput {
    pub path: PathBuf,
    pub modified_at_ns: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionRecommendation {
    pub path: PathBuf,
    pub score: i32,
    pub reasons: Vec<String>,
}

/// 对同组精确重复文件给出确定、可解释的保留顺序；评分只生成建议，不执行文件操作。
pub fn rank_duplicate_retention(inputs: &[RetentionInput]) -> Vec<RetentionRecommendation> {
    let newest = inputs.iter().map(|input| input.modified_at_ns).max();
    let shallowest = inputs
        .iter()
        .map(|input| input.path.components().count())
        .min();
    let mut recommendations = inputs
        .iter()
        .map(|input| {
            let mut score = 50_i32;
            let mut reasons = Vec::new();
            if Some(input.modified_at_ns) == newest {
                score += 25;
                reasons.push("同组中最近修改".to_owned());
            }
            if Some(input.path.components().count()) == shallowest {
                score += 15;
                reasons.push("路径层级较浅".to_owned());
            }
            let folded_name = input
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .nfkc()
                .flat_map(char::to_lowercase)
                .collect::<String>();
            if ["副本", "复制", "copy", "duplicate"]
                .iter()
                .any(|marker| folded_name.contains(marker))
            {
                score -= 25;
                reasons.push("名称包含副本标记".to_owned());
            } else {
                score += 10;
                reasons.push("名称无副本标记".to_owned());
            }
            if input.path.components().any(|component| {
                component
                    .as_os_str()
                    .to_str()
                    .is_some_and(|value| value.starts_with('.') && value != "." && value != "..")
            }) {
                score -= 20;
                reasons.push("位于隐藏目录或以点开头的路径".to_owned());
            }
            RetentionRecommendation {
                path: input.path.clone(),
                score: score.clamp(0, 100),
                reasons,
            }
        })
        .collect::<Vec<_>>();
    recommendations.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then(left.path.cmp(&right.path))
    });
    recommendations
}

#[derive(Debug, Error)]
pub enum AnalysisError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("图片解码失败：{0}")]
    Image(#[from] image::ImageError),
    #[error("相似度汉明距离上限必须在 0..=8，收到 {0}")]
    InvalidSimilarityDistance(u32),
}

/// 三级去重：大小分桶 → 首/中/尾快速指纹 → BLAKE3 全量校验。
pub fn find_exact_duplicates(paths: &[PathBuf]) -> DuplicateReport {
    let inputs = paths
        .iter()
        .filter_map(|path| {
            observe_file(path).ok().map(|observed| DuplicateInput {
                path: path.clone(),
                snapshot: observed.snapshot,
                cached_quick_fingerprint: None,
                cached_content_hash: None,
            })
        })
        .collect::<Vec<_>>();
    let mut result = find_exact_duplicates_cached(&inputs);
    result.report.stats.input_files = paths.len();
    result.report.stats.skipped_files += paths.len().saturating_sub(inputs.len());
    result.report
}

/// 使用调用方提供的快照绑定缓存；缓存只在磁盘快照仍一致时命中。
pub fn find_exact_duplicates_cached(inputs: &[DuplicateInput]) -> CachedDuplicateReport {
    find_exact_duplicates_cached_controlled(inputs, |_| true).result
}

/// 可协作中断的精确重复分析。中断时仍返回已经计算出的哈希，调用方可安全持久化并在恢复后复用。
pub fn find_exact_duplicates_cached_controlled(
    inputs: &[DuplicateInput],
    mut should_continue: impl FnMut(DuplicateProgress) -> bool,
) -> ControlledDuplicateReport {
    let mut stats = DuplicateStats {
        input_files: inputs.len(),
        ..DuplicateStats::default()
    };
    let mut by_size: HashMap<u64, Vec<&DuplicateInput>> = HashMap::new();
    for input in inputs {
        by_size.entry(input.snapshot.size).or_default().push(input);
    }
    let mut computed = HashMap::<PathBuf, ComputedFileHash>::new();
    let mut quick_groups: HashMap<(u64, [u8; 32]), Vec<&DuplicateInput>> = HashMap::new();
    // P5：仅计入大小桶 ≥2 的文件，避免进度条到不了 100%。
    let candidates_count: usize = by_size
        .values()
        .filter(|g| g.len() >= 2)
        .map(|g| g.len())
        .sum();
    let total_work = candidates_count.saturating_mul(2);
    let mut processed_files = 0_usize;
    let mut interrupted = false;
    for (size, candidates) in by_size {
        if candidates.len() < 2 {
            continue;
        }
        for input in candidates {
            if !should_continue(DuplicateProgress {
                phase: DuplicatePhase::QuickFingerprint,
                processed_files,
                total_work,
            }) {
                interrupted = true;
                break;
            }
            if !snapshot_matches(&input.path, &input.snapshot) {
                stats.skipped_files += 1;
                processed_files = processed_files.saturating_add(1);
                continue;
            }
            let quick = input.cached_quick_fingerprint.map_or_else(
                || quick_fingerprint(&input.path, size),
                |hash| {
                    stats.quick_cache_hits += 1;
                    Ok(hash)
                },
            );
            match quick {
                Ok(hash) => {
                    if input.cached_quick_fingerprint.is_none() {
                        stats.quick_fingerprinted_files += 1;
                        computed.insert(
                            input.path.clone(),
                            ComputedFileHash {
                                path: input.path.clone(),
                                quick_fingerprint: hash,
                                content_hash: None,
                            },
                        );
                    }
                    quick_groups.entry((size, hash)).or_default().push(input);
                }
                Err(_) => stats.skipped_files += 1,
            }
            processed_files = processed_files.saturating_add(1);
        }
        if interrupted {
            break;
        }
    }
    let mut exact: HashMap<(u64, [u8; 32]), Vec<PathBuf>> = HashMap::new();
    if !interrupted {
        'full_groups: for ((size, _), candidates) in quick_groups {
            if candidates.len() < 2 {
                continue;
            }
            for input in candidates {
                if !should_continue(DuplicateProgress {
                    phase: DuplicatePhase::FullHash,
                    processed_files,
                    total_work,
                }) {
                    interrupted = true;
                    break 'full_groups;
                }
                let full = input.cached_content_hash.map_or_else(
                    || verified_full_hash(&input.path),
                    |hash| {
                        stats.full_cache_hits += 1;
                        Ok(hash)
                    },
                );
                match full {
                    Ok(hash) => {
                        if input.cached_content_hash.is_none() {
                            stats.fully_hashed_files += 1;
                            let quick_fingerprint = input.cached_quick_fingerprint.or_else(|| {
                                computed
                                    .get(&input.path)
                                    .map(|entry| entry.quick_fingerprint)
                            });
                            computed
                                .entry(input.path.clone())
                                .and_modify(|entry| entry.content_hash = Some(hash))
                                .or_insert(ComputedFileHash {
                                    path: input.path.clone(),
                                    quick_fingerprint: quick_fingerprint
                                        .expect("full hashing requires a quick fingerprint"),
                                    content_hash: Some(hash),
                                });
                        }
                        exact
                            .entry((size, hash))
                            .or_default()
                            .push(input.path.clone());
                    }
                    Err(_) => stats.skipped_files += 1,
                }
                processed_files = processed_files.saturating_add(1);
            }
        }
    }
    let mut groups = exact
        .into_iter()
        .filter_map(|((size, content_hash), mut paths)| {
            if paths.len() < 2 {
                return None;
            }
            paths.sort();
            Some(DuplicateGroup {
                content_hash,
                size,
                paths,
            })
        })
        .collect::<Vec<_>>();
    groups.sort_by(|left, right| {
        right
            .size
            .cmp(&left.size)
            .then(left.paths.cmp(&right.paths))
    });
    ControlledDuplicateReport {
        result: CachedDuplicateReport {
            report: DuplicateReport { groups, stats },
            computed_hashes: computed.into_values().collect(),
        },
        interrupted,
    }
}

fn snapshot_matches(path: &Path, expected: &FileSnapshot) -> bool {
    observe_file(path)
        .map(|observed| {
            observed.snapshot.size == expected.size
                && observed.snapshot.modified_at_ns == expected.modified_at_ns
                && observed.snapshot.changed_at_ns == expected.changed_at_ns
        })
        .unwrap_or(false)
}

fn quick_fingerprint(path: &Path, size: u64) -> Result<[u8; 32], AnalysisError> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(&size.to_le_bytes());
    for offset in sample_offsets(size) {
        file.seek(SeekFrom::Start(offset))?;
        let remaining = size.saturating_sub(offset).min(SAMPLE_BYTES as u64) as usize;
        let mut buffer = vec![0; remaining];
        file.read_exact(&mut buffer)?;
        hasher.update(&offset.to_le_bytes());
        hasher.update(&buffer);
    }
    Ok(*hasher.finalize().as_bytes())
}

fn sample_offsets(size: u64) -> Vec<u64> {
    let sample = SAMPLE_BYTES as u64;
    let mut offsets = vec![0];
    if size > sample {
        offsets.push(size.saturating_sub(sample) / 2);
        offsets.push(size.saturating_sub(sample));
    }
    offsets.sort_unstable();
    offsets.dedup();
    offsets
}

fn verified_full_hash(path: &Path) -> Result<[u8; 32], AnalysisError> {
    let before = observe_file(path).map_err(|error| std::io::Error::other(error.to_string()))?;
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let after = observe_file(path).map_err(|error| std::io::Error::other(error.to_string()))?;
    if before != after {
        return Err(AnalysisError::Io(std::io::Error::other(
            "file changed while hashing",
        )));
    }
    Ok(*hasher.finalize().as_bytes())
}

/// 对自然语言文本生成 64 位 SimHash；字符三元组使中文和无空格文本也可比较。
pub fn text_simhash(text: &str) -> Option<u64> {
    let normalized = text.nfkc().flat_map(char::to_lowercase).collect::<String>();
    let characters = normalized
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<Vec<_>>();
    if characters.is_empty() {
        return None;
    }
    let mut features = Vec::new();
    if characters.len() < 3 {
        features.push(characters.iter().collect::<String>());
    } else {
        for window in characters.windows(3) {
            features.push(window.iter().collect::<String>());
        }
    }
    let mut weights = [0_i32; 64];
    for feature in features {
        let digest = blake3::hash(feature.as_bytes());
        let bits = u64::from_le_bytes(digest.as_bytes()[..8].try_into().expect("eight bytes"));
        for (index, weight) in weights.iter_mut().enumerate() {
            *weight += if bits & (1_u64 << index) == 0 { -1 } else { 1 };
        }
    }
    Some(
        weights
            .iter()
            .enumerate()
            .fold(0_u64, |hash, (index, weight)| {
                hash | (u64::from(*weight > 0) << index)
            }),
    )
}

pub const TEXT_MINHASH_COMPONENTS: usize = 32;
const MAX_MINHASH_FEATURES: usize = 32 * 1024;

/// 为文本字符五元组生成固定大小 MinHash 签名，用于 SimHash 候选的二次验证。
///
/// 特征按哈希值确定性截断，避免超大文本造成无界内存或 CPU 开销。
pub fn text_minhash(text: &str) -> Option<[u64; TEXT_MINHASH_COMPONENTS]> {
    let characters = text
        .nfkc()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect::<Vec<_>>();
    if characters.is_empty() {
        return None;
    }
    let shingles = if characters.len() < 5 {
        vec![characters.iter().collect::<String>()]
    } else {
        characters
            .windows(5)
            .map(|window| window.iter().collect::<String>())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    };
    let mut feature_hashes = shingles
        .into_iter()
        .map(|feature| {
            let digest = blake3::hash(feature.as_bytes());
            (
                u64::from_le_bytes(digest.as_bytes()[..8].try_into().expect("eight bytes")),
                u64::from_le_bytes(digest.as_bytes()[8..16].try_into().expect("eight bytes")) | 1,
            )
        })
        .collect::<Vec<_>>();
    feature_hashes.sort_unstable();
    feature_hashes.truncate(MAX_MINHASH_FEATURES);
    let mut signature = [u64::MAX; TEXT_MINHASH_COMPONENTS];
    for (first, second) in feature_hashes {
        for (index, minimum) in signature.iter_mut().enumerate() {
            let permuted = first
                .wrapping_add((index as u64).wrapping_mul(second))
                .rotate_left((index % 63) as u32);
            *minimum = (*minimum).min(permuted);
        }
    }
    Some(signature)
}

pub fn minhash_similarity(
    left: &[u64; TEXT_MINHASH_COMPONENTS],
    right: &[u64; TEXT_MINHASH_COMPONENTS],
) -> f32 {
    let equal = left
        .iter()
        .zip(right)
        .filter(|(left, right)| left == right)
        .count();
    equal as f32 / TEXT_MINHASH_COMPONENTS as f32
}

pub fn hash_similarity(left: u64, right: u64) -> f32 {
    1.0 - ((left ^ right).count_ones() as f32 / 64.0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimilarityHashInput {
    pub id: String,
    pub hash: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SimilarityCandidate {
    pub left_id: String,
    pub right_id: String,
    pub hamming_distance: u32,
    pub similarity: f32,
}

/// 一条已人工确认的相似度样本。主距离越小、二次相似度越高，越可能为近重复。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LabeledSimilaritySample {
    pub primary_distance: u32,
    pub secondary_similarity: f32,
    pub is_match: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThresholdMetrics {
    pub maximum_primary_distance: u32,
    pub minimum_secondary_similarity: f32,
    pub true_positives: usize,
    pub false_positives: usize,
    pub true_negatives: usize,
    pub false_negatives: usize,
    pub precision: f32,
    pub recall: f32,
    pub f1: f32,
    pub false_positive_rate: f32,
    pub accuracy: f32,
}

/// 评估双阶段门限。没有正预测或正样本时相应指标返回 0，避免 NaN 污染报告。
pub fn evaluate_similarity_threshold(
    samples: &[LabeledSimilaritySample],
    maximum_primary_distance: u32,
    minimum_secondary_similarity: f32,
) -> ThresholdMetrics {
    let minimum_secondary_similarity = minimum_secondary_similarity.clamp(0.0, 1.0);
    let mut true_positives = 0;
    let mut false_positives = 0;
    let mut true_negatives = 0;
    let mut false_negatives = 0;
    for sample in samples {
        let predicted = sample.primary_distance <= maximum_primary_distance
            && sample.secondary_similarity >= minimum_secondary_similarity;
        match (predicted, sample.is_match) {
            (true, true) => true_positives += 1,
            (true, false) => false_positives += 1,
            (false, false) => true_negatives += 1,
            (false, true) => false_negatives += 1,
        }
    }
    let precision = safe_ratio(true_positives, true_positives + false_positives);
    let recall = safe_ratio(true_positives, true_positives + false_negatives);
    let f1 = if precision + recall > 0.0 {
        2.0 * precision * recall / (precision + recall)
    } else {
        0.0
    };
    let false_positive_rate = safe_ratio(false_positives, false_positives + true_negatives);
    let accuracy = safe_ratio(true_positives + true_negatives, samples.len());
    ThresholdMetrics {
        maximum_primary_distance,
        minimum_secondary_similarity,
        true_positives,
        false_positives,
        true_negatives,
        false_negatives,
        precision,
        recall,
        f1,
        false_positive_rate,
        accuracy,
    }
}

/// 在调用方给出的候选网格上选择 F1 最优门限；并列时优先更高 precision，
/// 再优先更严格的主距离和二次相似度，避免无依据地扩大候选范围。
pub fn select_similarity_threshold(
    samples: &[LabeledSimilaritySample],
    primary_distances: &[u32],
    secondary_similarities: &[f32],
) -> Option<ThresholdMetrics> {
    select_similarity_threshold_with_precision_floor(
        samples,
        primary_distances,
        secondary_similarities,
        0.0,
    )
}

/// 在 F1 优化前施加最低 precision 门槛。文件清理相关场景应使用此入口，
/// 让误报安全约束优先于召回率；没有候选满足门槛时明确返回 None。
pub fn select_similarity_threshold_with_precision_floor(
    samples: &[LabeledSimilaritySample],
    primary_distances: &[u32],
    secondary_similarities: &[f32],
    minimum_precision: f32,
) -> Option<ThresholdMetrics> {
    let minimum_precision = minimum_precision.clamp(0.0, 1.0);
    primary_distances
        .iter()
        .flat_map(|&primary| {
            secondary_similarities
                .iter()
                .map(move |&secondary| evaluate_similarity_threshold(samples, primary, secondary))
        })
        .filter(|metrics| metrics.precision >= minimum_precision && metrics.true_positives > 0)
        .max_by(|left, right| {
            left.f1
                .total_cmp(&right.f1)
                .then(left.precision.total_cmp(&right.precision))
                .then(
                    right
                        .maximum_primary_distance
                        .cmp(&left.maximum_primary_distance),
                )
                .then(
                    left.minimum_secondary_similarity
                        .total_cmp(&right.minimum_secondary_similarity),
                )
        })
}

fn safe_ratio(numerator: usize, denominator: usize) -> f32 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f32 / denominator as f32
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RankedScore {
    pub id: String,
    /// 调用方归一化后的相关性，范围 0..=1；RRF 路径只使用列表顺序。
    pub score: f32,
}

/// 无标注数据时融合多个召回列表；只使用名次，避免混合不可比的原始分数。
pub fn reciprocal_rank_fusion(
    rankings: &[Vec<RankedScore>],
    rank_constant: f32,
) -> Vec<RankedScore> {
    let rank_constant = rank_constant.max(1.0);
    let mut fused = HashMap::<String, f32>::new();
    for ranking in rankings {
        for (index, item) in ranking.iter().enumerate() {
            *fused.entry(item.id.clone()).or_default() +=
                1.0 / (rank_constant + index as f32 + 1.0);
        }
    }
    sort_ranked_scores(fused)
}

/// 有少量本地域内标注后融合词法与语义归一化分数。
pub fn convex_rank_fusion(
    lexical: &[RankedScore],
    semantic: &[RankedScore],
    lexical_weight: f32,
) -> Vec<RankedScore> {
    let lexical_weight = lexical_weight.clamp(0.0, 1.0);
    let semantic_weight = 1.0 - lexical_weight;
    let mut fused = HashMap::<String, f32>::new();
    for item in lexical {
        *fused.entry(item.id.clone()).or_default() += item.score.clamp(0.0, 1.0) * lexical_weight;
    }
    for item in semantic {
        *fused.entry(item.id.clone()).or_default() += item.score.clamp(0.0, 1.0) * semantic_weight;
    }
    sort_ranked_scores(fused)
}

fn sort_ranked_scores(scores: HashMap<String, f32>) -> Vec<RankedScore> {
    let mut ranked = scores
        .into_iter()
        .map(|(id, score)| RankedScore { id, score })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then(left.id.cmp(&right.id))
    });
    ranked
}

fn hamming_masks(width: usize, radius: usize) -> Vec<u64> {
    let mut masks = vec![0_u64];
    if radius >= 1 {
        masks.extend((0..width).map(|bit| 1_u64 << bit));
    }
    if radius >= 2 {
        for left in 0..width {
            for right in left + 1..width {
                masks.push((1_u64 << left) | (1_u64 << right));
            }
        }
    }
    masks
}

/// 使用三段 Multi-Index Hashing 生成候选。半径不超过 8 时，三个分段中至少
/// 一个分段的距离不超过 2，因此枚举每段 0..=2 位邻域可完整召回，再以完整
/// 64 位汉明距离做最终过滤。
pub fn find_similarity_candidates(
    inputs: &[SimilarityHashInput],
    maximum_distance: u32,
) -> Result<Vec<SimilarityCandidate>, AnalysisError> {
    if maximum_distance > 8 {
        return Err(AnalysisError::InvalidSimilarityDistance(maximum_distance));
    }
    let segments = [(0_usize, 21_usize), (21, 21), (42, 22)];
    let masks = [
        hamming_masks(21, 2),
        hamming_masks(21, 2),
        hamming_masks(22, 2),
    ];
    let mut buckets = HashMap::<(usize, u64), Vec<usize>>::new();
    let mut candidate_pairs = HashSet::<(usize, usize)>::new();
    for (index, input) in inputs.iter().enumerate() {
        for (segment, &(offset, width)) in segments.iter().enumerate() {
            let value = (input.hash >> offset) & ((1_u64 << width) - 1);
            for &neighbor_mask in &masks[segment] {
                if let Some(matches) = buckets.get(&(segment, value ^ neighbor_mask)) {
                    candidate_pairs.extend(matches.iter().map(|&other| (other, index)));
                }
            }
            buckets.entry((segment, value)).or_default().push(index);
        }
    }
    let mut candidates = candidate_pairs
        .into_iter()
        .filter_map(|(left, right)| {
            let distance = (inputs[left].hash ^ inputs[right].hash).count_ones();
            (distance <= maximum_distance).then(|| SimilarityCandidate {
                left_id: inputs[left].id.clone(),
                right_id: inputs[right].id.clone(),
                hamming_distance: distance,
                similarity: 1.0 - distance as f32 / 64.0,
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.hamming_distance
            .cmp(&right.hamming_distance)
            .then(left.left_id.cmp(&right.left_id))
            .then(left.right_id.cmp(&right.right_id))
    });
    Ok(candidates)
}

/// 从调用方解码后的 9×8 灰度采样生成 dHash，避免算法层绑定具体图片解码器。
pub fn image_dhash(samples: &[u8; 72]) -> u64 {
    let mut hash = 0_u64;
    for row in 0..8 {
        for column in 0..8 {
            if samples[row * 9 + column] > samples[row * 9 + column + 1] {
                hash |= 1_u64 << (row * 8 + column);
            }
        }
    }
    hash
}

/// 在受限解码预算内读取常见图片，按 EXIF/容器方向归一化后生成 64 位灰度 dHash。
///
/// dHash 有意保留镜像、旋转差异；调用方可据此给出可解释的近似候选，
/// 但不得将它当作文件相同或可安全删除的依据。
pub fn image_dhash_from_path(path: &Path) -> Result<u64, AnalysisError> {
    Ok(image_hashes_from_path(path)?.0)
}

/// 一次解码同时生成 dHash 与 DCT pHash，供候选召回和二次验证分别使用。
pub fn image_hashes_from_path(path: &Path) -> Result<(u64, u64), AnalysisError> {
    let mut reader = ImageReader::open(path)?.with_guessed_format()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(32_768);
    limits.max_image_height = Some(32_768);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder.orientation()?;
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    let grayscale = image.resize_exact(9, 8, FilterType::Triangle).to_luma8();
    let mut samples = [0_u8; 72];
    samples.copy_from_slice(grayscale.as_raw());
    Ok((image_dhash(&samples), image_phash(&image)))
}

/// 32×32 灰度 DCT 的低频 8×8 pHash；DC 分量不参与阈值计算。
pub fn image_phash(image: &DynamicImage) -> u64 {
    const SOURCE: usize = 32;
    const LOW: usize = 8;
    let pixels = image
        .resize_exact(SOURCE as u32, SOURCE as u32, FilterType::Triangle)
        .to_luma8();
    let cosines = std::array::from_fn::<_, LOW, _>(|frequency| {
        std::array::from_fn::<_, SOURCE, _>(|coordinate| {
            ((std::f64::consts::PI / SOURCE as f64) * (coordinate as f64 + 0.5) * frequency as f64)
                .cos()
        })
    });
    let mut coefficients = [0_f64; LOW * LOW];
    for vertical in 0..LOW {
        for horizontal in 0..LOW {
            let mut sum = 0_f64;
            for y in 0..SOURCE {
                for x in 0..SOURCE {
                    sum += f64::from(pixels.get_pixel(x as u32, y as u32)[0])
                        * cosines[horizontal][x]
                        * cosines[vertical][y];
                }
            }
            coefficients[vertical * LOW + horizontal] = sum;
        }
    }
    let mut threshold_values = coefficients[1..].to_vec();
    threshold_values.sort_by(f64::total_cmp);
    let median = threshold_values[threshold_values.len() / 2];
    coefficients
        .iter()
        .enumerate()
        .skip(1)
        .fold(0_u64, |hash, (index, value)| {
            hash | (u64::from(*value > median) << index)
        })
}

const DAY_NS: i64 = 86_400_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CleanupPolicy {
    pub large_file_bytes: u64,
    pub old_file_age_ns: i64,
    pub stale_download_age_ns: i64,
}

impl Default for CleanupPolicy {
    fn default() -> Self {
        Self {
            large_file_bytes: 100 * 1024 * 1024,
            old_file_age_ns: 365 * DAY_NS,
            stale_download_age_ns: 180 * DAY_NS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CleanupReason {
    Large,
    Old,
    Temporary,
    StaleDownload,
}

/// 只根据索引快照生成可解释的清理候选，不读取内容，也不执行任何文件操作。
pub fn cleanup_reasons(
    path: &Path,
    size: u64,
    modified_at_ns: i64,
    now_ns: i64,
    policy: CleanupPolicy,
) -> Vec<CleanupReason> {
    let mut reasons = Vec::new();
    if size >= policy.large_file_bytes {
        reasons.push(CleanupReason::Large);
    }
    let age_ns = now_ns.saturating_sub(modified_at_ns).max(0);
    if age_ns >= policy.old_file_age_ns {
        reasons.push(CleanupReason::Old);
    }
    let folded_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .nfkc()
        .flat_map(char::to_lowercase)
        .collect::<String>();
    let temporary = [".tmp", ".temp", ".cache", ".log", ".bak", ".old"]
        .iter()
        .any(|suffix| folded_name.ends_with(suffix))
        || folded_name.ends_with('~');
    if temporary {
        reasons.push(CleanupReason::Temporary);
    }
    let in_downloads = path.components().any(|component| {
        component.as_os_str().to_str().is_some_and(|value| {
            matches!(
                value.to_lowercase().as_str(),
                "downloads" | "download" | "下载"
            )
        })
    });
    if in_downloads && age_ns >= policy.stale_download_age_ns {
        reasons.push(CleanupReason::StaleDownload);
    }
    reasons
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileCategory {
    Document,
    Image,
    Audio,
    Video,
    Archive,
    Code,
    Data,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Classification {
    pub category: FileCategory,
    pub confidence: f32,
    pub reason: &'static str,
}

pub fn classify_file(path: &Path, header: &[u8]) -> Classification {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if header.starts_with(b"PK\x03\x04") {
        let container_category = match extension.as_str() {
            "docx" | "docm" | "dotx" | "dotm" | "pptx" | "pptm" | "ppsx" | "potx" | "odt"
            | "odp" | "epub" | "pages" => Some(FileCategory::Document),
            "xlsx" | "xlsm" | "xltx" | "ods" | "numbers" => Some(FileCategory::Data),
            _ => None,
        };
        if let Some(category) = container_category {
            return Classification {
                category,
                confidence: 0.96,
                reason: "container_format",
            };
        }
    }
    let magic = if header.starts_with(b"%PDF-") {
        Some(FileCategory::Document)
    } else if header.starts_with(b"\x89PNG\r\n\x1a\n") || header.starts_with(b"\xff\xd8\xff") {
        Some(FileCategory::Image)
    } else if header.starts_with(b"PK\x03\x04") || header.starts_with(b"\x1f\x8b") {
        Some(FileCategory::Archive)
    } else {
        None
    };
    if let Some(category) = magic {
        return Classification {
            category,
            confidence: 0.99,
            reason: "magic_bytes",
        };
    }
    let category = match extension.as_str() {
        "md" | "txt" | "rtf" | "pdf" | "doc" | "docx" | "docm" | "dotx" | "ppt" | "pptx"
        | "pptm" | "ppsx" | "potx" | "pages" | "odt" | "odp" | "epub" => FileCategory::Document,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" => FileCategory::Image,
        "mp3" | "wav" | "flac" | "m4a" | "aac" => FileCategory::Audio,
        "mp4" | "mov" | "mkv" | "avi" | "webm" => FileCategory::Video,
        "zip" | "gz" | "7z" | "rar" | "tar" => FileCategory::Archive,
        "rs" | "py" | "js" | "ts" | "go" | "java" | "c" | "cpp" | "swift" => FileCategory::Code,
        "csv" | "tsv" | "json" | "xml" | "yaml" | "yml" | "toml" | "xls" | "xlsx" | "xlsm"
        | "xltx" | "numbers" | "ods" => FileCategory::Data,
        _ => FileCategory::Other,
    };
    Classification {
        category,
        confidence: if category == FileCategory::Other {
            0.2
        } else {
            0.82
        },
        reason: "extension",
    }
}

pub fn safe_filename(stem: &str, extension: Option<&str>) -> String {
    let mut normalized = stem
        .nfc()
        .filter(|character| {
            !character.is_control()
                && !matches!(
                    character,
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
                )
        })
        .collect::<String>();
    normalized = normalized
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches([' ', '.'])
        .to_owned();
    if normalized.is_empty() {
        normalized = "未命名文件".to_owned();
    }
    let reserved = [
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    if reserved.contains(&normalized.to_ascii_lowercase().as_str()) {
        normalized.insert(0, '_');
    }
    // 总长上限 250 字节（macOS NAME_MAX=255，留 5 字节余量给冲突后缀）。
    let extension = extension
        .map(|value| value.trim_start_matches('.'))
        .filter(|value| !value.is_empty());
    let ext_len = extension.map_or(0, |ext| ext.len() + 1); // +1 for '.'
    while normalized.len() + ext_len > 250 {
        if normalized.is_empty() {
            break;
        }
        normalized.pop();
    }
    match extension {
        Some(ext) => format!("{normalized}.{ext}"),
        None => normalized,
    }
}

pub fn unique_filename(desired: &str, existing: &HashSet<String>) -> String {
    let folded = existing
        .iter()
        .map(|name| name.to_lowercase())
        .collect::<HashSet<_>>();
    if !folded.contains(&desired.to_lowercase()) {
        return desired.to_owned();
    }
    let path = Path::new(desired);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(desired);
    let extension = path.extension().and_then(|value| value.to_str());
    for index in 2..=100_000 {
        let candidate = safe_filename(&format!("{stem} ({index})"), extension);
        if !folded.contains(&candidate.to_lowercase()) {
            return candidate;
        }
    }
    safe_filename(
        &format!("{stem}-{}", blake3::hash(desired.as_bytes()).to_hex()),
        extension,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_duplicates_hash_only_size_and_quick_candidates() {
        let directory = tempfile::tempdir().expect("temp directory");
        let a = directory.path().join("a.bin");
        let b = directory.path().join("b.bin");
        let unique = directory.path().join("unique.bin");
        std::fs::write(&a, b"same content").expect("a");
        std::fs::write(&b, b"same content").expect("b");
        std::fs::write(&unique, b"x").expect("unique");
        let report = find_exact_duplicates(&[a.clone(), b.clone(), unique]);
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].paths, vec![a, b]);
        assert_eq!(report.stats.fully_hashed_files, 2);
    }

    #[test]
    fn exact_duplicates_reuses_snapshot_bound_hashes() {
        let directory = tempfile::tempdir().expect("temp directory");
        let a = directory.path().join("a.bin");
        let b = directory.path().join("b.bin");
        std::fs::write(&a, b"same cached content").expect("a");
        std::fs::write(&b, b"same cached content").expect("b");
        let uncached = [&a, &b]
            .into_iter()
            .map(|path| DuplicateInput {
                path: path.clone(),
                snapshot: observe_file(path).expect("snapshot").snapshot,
                cached_quick_fingerprint: None,
                cached_content_hash: None,
            })
            .collect::<Vec<_>>();
        let first = find_exact_duplicates_cached(&uncached);
        let hashes = first
            .computed_hashes
            .into_iter()
            .map(|hash| (hash.path.clone(), hash))
            .collect::<HashMap<_, _>>();
        let cached = uncached
            .into_iter()
            .map(|input| {
                let hash = &hashes[&input.path];
                DuplicateInput {
                    cached_quick_fingerprint: Some(hash.quick_fingerprint),
                    cached_content_hash: hash.content_hash,
                    ..input
                }
            })
            .collect::<Vec<_>>();
        let second = find_exact_duplicates_cached(&cached);
        assert_eq!(second.report.groups.len(), 1);
        assert_eq!(second.report.stats.quick_cache_hits, 2);
        assert_eq!(second.report.stats.full_cache_hits, 2);
        assert_eq!(second.report.stats.quick_fingerprinted_files, 0);
        assert_eq!(second.report.stats.fully_hashed_files, 0);
        assert!(second.computed_hashes.is_empty());
    }

    #[test]
    fn exact_duplicate_analysis_interrupts_cooperatively_and_returns_partial_cache() {
        let directory = tempfile::tempdir().expect("temp directory");
        let paths = ["a.bin", "b.bin", "c.bin"]
            .into_iter()
            .map(|name| {
                let path = directory.path().join(name);
                std::fs::write(&path, b"same content for controlled analysis").expect("file");
                path
            })
            .collect::<Vec<_>>();
        let inputs = paths
            .iter()
            .map(|path| DuplicateInput {
                path: path.clone(),
                snapshot: observe_file(path).expect("snapshot").snapshot,
                cached_quick_fingerprint: None,
                cached_content_hash: None,
            })
            .collect::<Vec<_>>();
        let controlled = find_exact_duplicates_cached_controlled(&inputs, |progress| {
            progress.processed_files < 1
        });
        assert!(controlled.interrupted);
        assert_eq!(controlled.result.computed_hashes.len(), 1);
        assert!(controlled.result.report.groups.is_empty());
    }

    #[test]
    fn retention_ranking_penalizes_copy_markers_and_explains_the_choice() {
        let ranked = rank_duplicate_retention(&[
            RetentionInput {
                path: PathBuf::from("/资料/合同.pdf"),
                modified_at_ns: 10,
            },
            RetentionInput {
                path: PathBuf::from("/资料/合同 copy.pdf"),
                modified_at_ns: 20,
            },
            RetentionInput {
                path: PathBuf::from("/资料/.cache/合同.pdf"),
                modified_at_ns: 5,
            },
        ]);
        assert_eq!(ranked[0].path, PathBuf::from("/资料/合同.pdf"));
        assert!(
            ranked[0]
                .reasons
                .iter()
                .any(|reason| reason == "名称无副本标记")
        );
        assert!(
            ranked[1]
                .reasons
                .iter()
                .any(|reason| reason == "名称包含副本标记")
        );
        assert!(
            ranked[2]
                .reasons
                .iter()
                .any(|reason| reason.contains("隐藏目录"))
        );
    }

    #[test]
    fn simhash_rates_small_chinese_edit_as_similar() {
        let left = text_simhash("项目计划和预算说明").expect("left feature");
        let right = text_simhash("项目计划与预算说明").expect("right feature");
        assert!(hash_similarity(left, right) > 0.70);
    }

    #[test]
    fn simhash_rejects_text_without_features() {
        assert_eq!(text_simhash("  ——  "), None);
    }

    #[test]
    fn minhash_secondary_verification_distinguishes_related_text() {
        let original =
            text_minhash("项目计划包含预算、时间表、交付标准和风险说明").expect("original");
        let edited =
            text_minhash("项目计划包含预算、时间安排、交付标准以及风险说明").expect("edited");
        let unrelated = text_minhash("周末天气晴朗，适合徒步和拍摄城市风景").expect("unrelated");
        assert!(minhash_similarity(&original, &edited) > minhash_similarity(&original, &unrelated));
        assert_eq!(minhash_similarity(&original, &original), 1.0);
    }

    #[test]
    fn similarity_candidate_index_recalls_close_hashes_without_all_pairs() {
        let candidates = find_similarity_candidates(
            &[
                SimilarityHashInput {
                    id: "a".to_owned(),
                    hash: 0,
                },
                SimilarityHashInput {
                    id: "b".to_owned(),
                    hash: 0b10101,
                },
                SimilarityHashInput {
                    id: "far".to_owned(),
                    hash: u64::MAX,
                },
            ],
            5,
        )
        .expect("valid distance");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].left_id, "a");
        assert_eq!(candidates[0].right_id, "b");
        assert_eq!(candidates[0].hamming_distance, 3);
    }

    #[test]
    fn similarity_candidate_index_rejects_unsupported_distance() {
        assert!(matches!(
            find_similarity_candidates(&[], 9),
            Err(AnalysisError::InvalidSimilarityDistance(9))
        ));
    }

    #[test]
    fn similarity_multi_index_matches_exhaustive_hamming_search() {
        let inputs = (0..512_u64)
            .map(|index| SimilarityHashInput {
                id: format!("item-{index:04}"),
                hash: {
                    let mut value = index.wrapping_add(0x9e3779b97f4a7c15);
                    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
                    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
                    value ^ (value >> 31)
                },
            })
            .collect::<Vec<_>>();
        let actual = find_similarity_candidates(&inputs, 8).expect("valid radius");
        let expected = inputs
            .iter()
            .enumerate()
            .flat_map(|(left, left_input)| {
                inputs
                    .iter()
                    .enumerate()
                    .skip(left + 1)
                    .filter_map(move |(_, right_input)| {
                        let distance = (left_input.hash ^ right_input.hash).count_ones();
                        (distance <= 8).then_some((
                            left_input.id.clone(),
                            right_input.id.clone(),
                            distance,
                        ))
                    })
            })
            .collect::<HashSet<_>>();
        let actual = actual
            .into_iter()
            .map(|candidate| {
                (
                    candidate.left_id,
                    candidate.right_id,
                    candidate.hamming_distance,
                )
            })
            .collect::<HashSet<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn threshold_evaluation_reports_confusion_matrix_and_metrics() {
        let samples = [
            LabeledSimilaritySample {
                primary_distance: 2,
                secondary_similarity: 0.91,
                is_match: true,
            },
            LabeledSimilaritySample {
                primary_distance: 7,
                secondary_similarity: 0.74,
                is_match: true,
            },
            LabeledSimilaritySample {
                primary_distance: 4,
                secondary_similarity: 0.20,
                is_match: false,
            },
            LabeledSimilaritySample {
                primary_distance: 10,
                secondary_similarity: 0.88,
                is_match: false,
            },
        ];
        let metrics = evaluate_similarity_threshold(&samples, 8, 0.50);
        assert_eq!(metrics.true_positives, 2);
        assert_eq!(metrics.false_positives, 0);
        assert_eq!(metrics.true_negatives, 2);
        assert_eq!(metrics.false_negatives, 0);
        assert_eq!(metrics.precision, 1.0);
        assert_eq!(metrics.recall, 1.0);
        assert_eq!(metrics.f1, 1.0);
        assert_eq!(metrics.false_positive_rate, 0.0);
        assert_eq!(metrics.accuracy, 1.0);
    }

    #[test]
    fn threshold_selection_prefers_stricter_equal_quality_gate() {
        let samples = [
            LabeledSimilaritySample {
                primary_distance: 3,
                secondary_similarity: 0.80,
                is_match: true,
            },
            LabeledSimilaritySample {
                primary_distance: 9,
                secondary_similarity: 0.95,
                is_match: false,
            },
        ];
        let selected = select_similarity_threshold(&samples, &[8, 6, 4], &[0.25, 0.50])
            .expect("non-empty threshold grid");
        assert_eq!(selected.maximum_primary_distance, 4);
        assert_eq!(selected.minimum_secondary_similarity, 0.50);
        assert_eq!(selected.f1, 1.0);
    }

    #[test]
    fn precision_floor_rejects_unsafe_high_recall_thresholds() {
        let samples = [
            LabeledSimilaritySample {
                primary_distance: 2,
                secondary_similarity: 0.90,
                is_match: true,
            },
            LabeledSimilaritySample {
                primary_distance: 6,
                secondary_similarity: 0.45,
                is_match: true,
            },
            LabeledSimilaritySample {
                primary_distance: 4,
                secondary_similarity: 0.40,
                is_match: false,
            },
        ];
        let selected = select_similarity_threshold_with_precision_floor(
            &samples,
            &[4, 6],
            &[0.35, 0.80],
            0.90,
        )
        .expect("strict safe threshold");
        assert_eq!(selected.maximum_primary_distance, 4);
        assert_eq!(selected.minimum_secondary_similarity, 0.80);
        assert_eq!(selected.precision, 1.0);
        assert!(selected.recall < 1.0);
    }

    #[test]
    fn hybrid_rank_fusion_is_deterministic_and_does_not_mix_raw_score_scales() {
        let lexical = vec![
            RankedScore {
                id: "exact-name".to_owned(),
                score: 1.0,
            },
            RankedScore {
                id: "shared".to_owned(),
                score: 0.6,
            },
        ];
        let semantic = vec![
            RankedScore {
                id: "shared".to_owned(),
                score: 0.9,
            },
            RankedScore {
                id: "meaning".to_owned(),
                score: 0.8,
            },
        ];
        let rrf = reciprocal_rank_fusion(&[lexical.clone(), semantic.clone()], 60.0);
        assert_eq!(rrf[0].id, "shared");
        let convex = convex_rank_fusion(&lexical, &semantic, 0.7);
        assert_eq!(convex[0].id, "exact-name");
        assert!(convex.iter().all(|item| item.score.is_finite()));
    }

    #[test]
    fn magic_bytes_override_a_misleading_extension() {
        let result = classify_file(Path::new("photo.txt"), b"\x89PNG\r\n\x1a\nrest");
        assert_eq!(result.category, FileCategory::Image);
        assert_eq!(result.reason, "magic_bytes");
    }

    #[test]
    fn zip_container_documents_are_not_misclassified_as_archives() {
        let docx = classify_file(Path::new("contract.docx"), b"PK\x03\x04rest");
        assert_eq!(docx.category, FileCategory::Document);
        assert_eq!(docx.reason, "container_format");

        let xlsx = classify_file(Path::new("budget.xlsx"), b"PK\x03\x04rest");
        assert_eq!(xlsx.category, FileCategory::Data);
        assert_eq!(xlsx.reason, "container_format");

        let zip = classify_file(Path::new("backup.zip"), b"PK\x03\x04rest");
        assert_eq!(zip.category, FileCategory::Archive);
        assert_eq!(zip.reason, "magic_bytes");
    }

    #[test]
    fn names_are_safe_reserved_aware_and_case_insensitively_unique() {
        assert_eq!(safe_filename("  CON  ", Some("txt")), "_CON.txt");
        let existing = HashSet::from(["报告.PDF".to_owned(), "报告 (2).pdf".to_owned()]);
        assert_eq!(unique_filename("报告.pdf", &existing), "报告 (3).pdf");
    }

    #[test]
    fn cleanup_analysis_is_explainable_and_boundary_inclusive() {
        let now = 500 * DAY_NS;
        let reasons = cleanup_reasons(
            Path::new("/Users/test/Downloads/archive.tmp"),
            100 * 1024 * 1024,
            now - 365 * DAY_NS,
            now,
            CleanupPolicy::default(),
        );
        assert_eq!(
            reasons,
            vec![
                CleanupReason::Large,
                CleanupReason::Old,
                CleanupReason::Temporary,
                CleanupReason::StaleDownload,
            ]
        );

        assert!(
            cleanup_reasons(
                Path::new("/Users/test/Documents/current.txt"),
                1024,
                now - DAY_NS,
                now,
                CleanupPolicy::default(),
            )
            .is_empty()
        );
    }

    #[test]
    fn cleanup_analysis_does_not_treat_future_timestamps_as_old() {
        let now = 10 * DAY_NS;
        let reasons = cleanup_reasons(
            Path::new("/home/test/Downloads/future.txt"),
            1024,
            now + DAY_NS,
            now,
            CleanupPolicy::default(),
        );
        assert!(reasons.is_empty());
    }

    #[test]
    fn image_dhash_is_stable_and_distance_compatible() {
        let mut samples = [0_u8; 72];
        for row in 0..8 {
            for col in 0..9 {
                samples[row * 9 + col] = (8 - col) as u8;
            }
        }
        assert_eq!(image_dhash(&samples), u64::MAX);
        assert_eq!(
            hash_similarity(image_dhash(&samples), image_dhash(&samples)),
            1.0
        );
    }

    #[test]
    fn image_dhash_decodes_png_with_bounded_pipeline() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("gradient.png");
        let image = image::GrayImage::from_fn(90, 80, |x, _| image::Luma([(255 - x) as u8]));
        image.save(&path)?;

        assert_eq!(image_dhash_from_path(&path)?, u64::MAX);
        Ok(())
    }

    #[test]
    fn image_phash_survives_resize_and_jpeg_encoding() -> Result<(), Box<dyn std::error::Error>> {
        use image::codecs::jpeg::JpegEncoder;

        let source = image::RgbImage::from_fn(160, 120, |x, y| {
            let block = if (x / 24 + y / 20) % 2 == 0 { 220 } else { 35 };
            image::Rgb([block, ((x + y) % 255) as u8, 255 - block])
        });
        let resized = image::imageops::resize(&source, 96, 72, FilterType::Lanczos3);
        let directory = tempfile::tempdir()?;
        let png = directory.path().join("source.png");
        let jpeg = directory.path().join("resized.jpg");
        source.save(&png)?;
        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, 82)
            .encode_image(&DynamicImage::ImageRgb8(resized))?;
        std::fs::write(&jpeg, bytes)?;

        let (_, original_phash) = image_hashes_from_path(&png)?;
        let (_, transformed_phash) = image_hashes_from_path(&jpeg)?;
        assert!((original_phash ^ transformed_phash).count_ones() <= 10);
        Ok(())
    }

    #[test]
    fn image_dhash_normalizes_exif_orientation() -> Result<(), Box<dyn std::error::Error>> {
        use image::codecs::jpeg::JpegEncoder;

        fn jpeg_bytes(image: &image::RgbImage) -> Result<Vec<u8>, image::ImageError> {
            let mut bytes = Vec::new();
            JpegEncoder::new_with_quality(&mut bytes, 100)
                .encode_image(&image::DynamicImage::ImageRgb8(image.clone()))?;
            Ok(bytes)
        }

        fn with_orientation_six(jpeg: Vec<u8>) -> Vec<u8> {
            // APP1 Exif：little-endian TIFF，Orientation(0x0112)=6（显示时顺时针 90°）。
            let exif = [
                0xff, 0xe1, 0x00, 0x22, b'E', b'x', b'i', b'f', 0x00, 0x00, b'I', b'I', 0x2a, 0x00,
                0x08, 0x00, 0x00, 0x00, 0x01, 0x00, 0x12, 0x01, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00,
                0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ];
            let mut oriented = Vec::with_capacity(jpeg.len() + exif.len());
            oriented.extend_from_slice(&jpeg[..2]);
            oriented.extend_from_slice(&exif);
            oriented.extend_from_slice(&jpeg[2..]);
            oriented
        }

        let directory = tempfile::tempdir()?;
        let upright_path = directory.path().join("upright.jpg");
        let oriented_path = directory.path().join("oriented.jpg");
        let upright = image::RgbImage::from_fn(90, 60, |x, y| {
            image::Rgb([(x * 2) as u8, (y * 3) as u8, ((x + y) % 255) as u8])
        });
        let stored_rotated = image::imageops::rotate270(&upright);
        std::fs::write(&upright_path, jpeg_bytes(&upright)?)?;
        std::fs::write(
            &oriented_path,
            with_orientation_six(jpeg_bytes(&stored_rotated)?),
        )?;

        let upright_hash = image_dhash_from_path(&upright_path)?;
        let oriented_hash = image_dhash_from_path(&oriented_path)?;
        assert!(
            (upright_hash ^ oriented_hash).count_ones() <= 2,
            "EXIF 方向归一化后的感知哈希应保持一致"
        );
        Ok(())
    }
}
