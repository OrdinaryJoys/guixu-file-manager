//! 可验证、确定性且完全本地的文件分析算法。

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use guixu_platform::observe_file;
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

const SAMPLE_BYTES: usize = 64 * 1024;

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
    pub skipped_files: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateReport {
    pub groups: Vec<DuplicateGroup>,
    pub stats: DuplicateStats,
}

#[derive(Debug, Error)]
pub enum AnalysisError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// 三级去重：大小分桶 → 首/中/尾快速指纹 → BLAKE3 全量校验。
pub fn find_exact_duplicates(paths: &[PathBuf]) -> DuplicateReport {
    let mut stats = DuplicateStats {
        input_files: paths.len(),
        ..DuplicateStats::default()
    };
    let mut by_size: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    for path in paths {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                by_size
                    .entry(metadata.len())
                    .or_default()
                    .push(path.clone());
            }
            _ => stats.skipped_files += 1,
        }
    }
    let mut quick_groups: HashMap<(u64, [u8; 32]), Vec<PathBuf>> = HashMap::new();
    for (size, candidates) in by_size {
        if candidates.len() < 2 {
            continue;
        }
        for path in candidates {
            match quick_fingerprint(&path, size) {
                Ok(hash) => {
                    stats.quick_fingerprinted_files += 1;
                    quick_groups.entry((size, hash)).or_default().push(path);
                }
                Err(_) => stats.skipped_files += 1,
            }
        }
    }
    let mut exact: HashMap<(u64, [u8; 32]), Vec<PathBuf>> = HashMap::new();
    for ((size, _), candidates) in quick_groups {
        if candidates.len() < 2 {
            continue;
        }
        for path in candidates {
            match verified_full_hash(&path) {
                Ok(hash) => {
                    stats.fully_hashed_files += 1;
                    exact.entry((size, hash)).or_default().push(path);
                }
                Err(_) => stats.skipped_files += 1,
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
    DuplicateReport { groups, stats }
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
pub fn text_simhash(text: &str) -> u64 {
    let normalized = text.nfkc().flat_map(char::to_lowercase).collect::<String>();
    let characters = normalized
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<Vec<_>>();
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
    weights
        .iter()
        .enumerate()
        .fold(0_u64, |hash, (index, weight)| {
            hash | (u64::from(*weight >= 0) << index)
        })
}

pub fn hash_similarity(left: u64, right: u64) -> f32 {
    1.0 - ((left ^ right).count_ones() as f32 / 64.0)
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
            | "odp" | "epub" => Some(FileCategory::Document),
            "xlsx" | "xlsm" | "xltx" | "ods" => Some(FileCategory::Data),
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
    while normalized.len() > 220 {
        normalized.pop();
    }
    match extension
        .map(|value| value.trim_start_matches('.'))
        .filter(|value| !value.is_empty())
    {
        Some(extension) => format!("{normalized}.{extension}"),
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
    fn simhash_rates_small_chinese_edit_as_similar() {
        let left = text_simhash("项目计划和预算说明");
        let right = text_simhash("项目计划与预算说明");
        assert!(hash_similarity(left, right) > 0.70);
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
}
