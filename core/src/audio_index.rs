//! 本地音频索引：元数据分析、Chromaprint 指纹、重复检测与以声搜声。
//!
//! 索引由应用配置目录中的 JSON 持久化；文件签名包含路径、大小和修改时间，
//! 因此目录监听只需重新分析新增或变化的文件。指纹永不发送到网络。
//!
//! 同步被拆成 [`AudioIndexStore::plan_sync`]（只读、毫秒级）、[`analyze_plan`]
//! （解码分析，可跑几分钟、不持锁）与 [`AudioIndexStore::apply_sync`]（写回）
//! 三步，调用方因此不必在 ffmpeg 运行期间占用索引锁。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::categories::Category;
use crate::library::{Song, has_audio_extension};
use crate::paths::strip_verbatim_prefix;
use crate::persist::write_json_compact;
use crate::process::command;

pub const INDEX_VERSION: u32 = 2;
pub const ANALYSIS_SAMPLE_RATE: u32 = 11_025;
pub const WAVEFORM_BUCKETS: usize = 800;
/// Chromaprint 相邻子指纹的时间间隔：实测 463 帧 / 60 秒，即 8.07 帧/秒。
pub const FINGERPRINT_FRAME_SECONDS: f64 = 0.123_97;
/// 出现次数超过该值的指纹哈希视为「过度常见」（静音、持续音），跳过以免投票空间爆炸。
/// 代价是同一段音频存在数百份副本时会漏检，本地 BGM 曲库远达不到这个量级。
const MAX_COMMON_POSTINGS: usize = 256;
/// 单次同步最多写入多少条逐条告警，其余折叠为一条汇总，避免千级失败撑爆界面。
const MAX_REPORTED_WARNINGS: usize = 12;
/// 同一份损坏文件的分析失败在 10 分钟内不重试，避免兜底扫描每 30 秒拉起一次 ffmpeg。
const FAILED_RETRY_MS: u64 = 10 * 60 * 1_000;

/// 索引缓存的落盘编码。
///
/// 指纹与波形是每首曲目最大的两块数据：若按十进制数字写进 JSON，2000 首的索引会到
/// 50 MB 量级，启动时还要解析数百万个数字。改成 Base64 字符串后体积降到约 11 KB/首，
/// 解析成本从「逐数字」变成「逐字符串」，大曲库的冷启动明显更快。
mod codec {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let triple = ((chunk[0] as u32) << 16)
                | ((chunk.get(1).copied().unwrap_or(0) as u32) << 8)
                | chunk.get(2).copied().unwrap_or(0) as u32;
            out.push(ALPHABET[(triple >> 18) as usize & 63] as char);
            out.push(ALPHABET[(triple >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 {
                ALPHABET[(triple >> 6) as usize & 63] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                ALPHABET[triple as usize & 63] as char
            } else {
                '='
            });
        }
        out
    }

    fn decode(text: &str) -> Result<Vec<u8>, &'static str> {
        let mut out = Vec::with_capacity(text.len() / 4 * 3);
        let mut accumulator = 0_u32;
        let mut bits = 0_u32;
        for byte in text.bytes() {
            if byte == b'=' || byte.is_ascii_whitespace() {
                continue;
            }
            let value = match byte {
                b'A'..=b'Z' => byte - b'A',
                b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => return Err("索引缓存包含非法 Base64 字符"),
            } as u32;
            accumulator = (accumulator << 6) | value;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((accumulator >> bits) as u8);
                accumulator &= (1 << bits) - 1;
            }
        }
        Ok(out)
    }

    /// 指纹：每个子指纹 4 字节小端，整体 Base64。
    pub mod fingerprint {
        use super::{decode, encode};
        use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

        pub fn serialize<S>(value: &[u32], serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            let mut bytes = Vec::with_capacity(value.len() * 4);
            for word in value {
                bytes.extend_from_slice(&word.to_le_bytes());
            }
            serializer.serialize_str(&encode(&bytes))
        }

        pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u32>, D::Error>
        where
            D: Deserializer<'de>,
        {
            let text = String::deserialize(deserializer)?;
            let bytes = decode(&text).map_err(D::Error::custom)?;
            if bytes.len() % 4 != 0 {
                return Err(D::Error::custom("索引指纹长度不是 4 的倍数"));
            }
            Ok(bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|chunk| u32::from_le_bytes(*chunk))
                .collect())
        }
    }

    /// 波形：每个采样点量化成 1 字节（0–255），画图时再还原到 0–1。
    pub mod waveform {
        use super::{decode, encode};
        use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

        pub fn serialize<S>(value: &[f32], serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            let bytes: Vec<u8> = value
                .iter()
                .map(|peak| (peak.clamp(0.0, 1.0) * 255.0).round() as u8)
                .collect();
            serializer.serialize_str(&encode(&bytes))
        }

        pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<f32>, D::Error>
        where
            D: Deserializer<'de>,
        {
            let text = String::deserialize(deserializer)?;
            let bytes = decode(&text).map_err(D::Error::custom)?;
            Ok(bytes.iter().map(|value| *value as f32 / 255.0).collect())
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexEntry {
    pub id: String,
    pub key: Option<String>,
    pub title: String,
    pub file_name: String,
    pub category: String,
    pub path: String,
    pub size_bytes: u64,
    pub modified_ms: u64,
    pub duration_ms: Option<u32>,
    pub bpm: Option<f64>,
    pub musical_key: Option<String>,
    pub integrated_lufs: Option<f64>,
    pub peak_db: Option<f64>,
    pub true_peak_db: Option<f64>,
    #[serde(with = "codec::waveform", default)]
    pub waveform: Vec<f32>,
    #[serde(with = "codec::fingerprint", default)]
    pub fingerprint: Vec<u32>,
    pub analyzed_at_ms: u64,
    pub error: Option<String>,
}

impl IndexEntry {
    /// 文件签名（路径 + 大小 + 修改时间）是否仍与候选文件一致。
    fn same_source(&self, candidate: &IndexCandidate) -> bool {
        self.path == candidate.id
            && self.size_bytes == candidate.size_bytes
            && self.modified_ms == candidate.modified_ms
    }

    fn signature_matches(&self, candidate: &IndexCandidate) -> bool {
        self.same_source(candidate) && self.error.is_none() && !self.fingerprint.is_empty()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioIndexStore {
    pub version: u32,
    pub root: String,
    /// 用户确认后持续维护当前曲库；仅写索引文件，不修改音频。
    #[serde(default)]
    pub enabled: bool,
    pub entries: BTreeMap<String, IndexEntry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioIndexSummary {
    pub id: String,
    pub key: Option<String>,
    pub title: String,
    pub file_name: String,
    pub category: String,
    pub path: String,
    pub size_bytes: u64,
    pub duration_ms: Option<u32>,
    pub bpm: Option<f64>,
    pub musical_key: Option<String>,
    pub integrated_lufs: Option<f64>,
    pub peak_db: Option<f64>,
    pub analyzed_at_ms: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioIndexInfo {
    #[serde(flatten)]
    pub summary: AudioIndexSummary,
    pub waveform: Vec<f32>,
    pub true_peak_db: Option<f64>,
}

impl IndexEntry {
    pub fn summary(&self) -> AudioIndexSummary {
        AudioIndexSummary {
            id: self.id.clone(),
            key: self.key.clone(),
            title: self.title.clone(),
            file_name: self.file_name.clone(),
            category: self.category.clone(),
            path: self.path.clone(),
            size_bytes: self.size_bytes,
            duration_ms: self.duration_ms,
            bpm: self.bpm,
            musical_key: self.musical_key.clone(),
            integrated_lufs: self.integrated_lufs,
            peak_db: self.peak_db,
            analyzed_at_ms: self.analyzed_at_ms,
            error: self.error.clone(),
        }
    }

    pub fn info(&self) -> AudioIndexInfo {
        AudioIndexInfo {
            summary: self.summary(),
            waveform: self.waveform.clone(),
            true_peak_db: self.true_peak_db,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexSyncReport {
    pub analyzed: usize,
    pub reused: usize,
    pub removed: usize,
    pub failed: usize,
    pub warnings: Vec<String>,
    /// 内存中的索引是否真的发生了变化；调用方据此决定要不要保存与通知前端。
    #[serde(skip)]
    pub changed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateTrack {
    pub id: String,
    pub key: Option<String>,
    pub title: String,
    pub file_name: String,
    pub category: String,
    pub path: String,
    pub size_bytes: u64,
    pub duration_ms: Option<u32>,
    pub bpm: Option<f64>,
    pub musical_key: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateGroup {
    pub id: String,
    pub match_kind: &'static str,
    pub score: f64,
    pub tracks: Vec<DuplicateTrack>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateReport {
    pub groups: Vec<DuplicateGroup>,
    pub indexed_tracks: usize,
    pub failed_tracks: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SoundMatch {
    #[serde(flatten)]
    pub track: DuplicateTrack,
    pub score: f64,
    pub offset_seconds: f64,
    pub matched_frames: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SoundSearchResult {
    pub query_path: String,
    pub matches: Vec<SoundMatch>,
}

#[derive(Debug, Clone)]
struct IndexCandidate {
    id: String,
    key: Option<String>,
    title: String,
    file_name: String,
    category: String,
    size_bytes: u64,
    modified_ms: u64,
    duration_ms: Option<u32>,
}

#[derive(Debug, Error)]
pub enum AudioIndexError {
    #[error("读取音频索引失败：{0}")]
    Read(#[source] std::io::Error),
    #[error("保存音频索引失败：{0}")]
    Persist(#[source] crate::persist::PersistError),
    #[error("音频分析失败：{0}")]
    Analyze(String),
}

/// 旧缓存整份解析失败时，只救回「是否启用」与根目录，避免用户被迫重新确认。
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct IndexMetadata {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    root: String,
}

impl AudioIndexStore {
    pub fn load(path: &Path) -> Result<Self, AudioIndexError> {
        match fs::read(path) {
            Ok(bytes) => {
                let mut store: Self = match serde_json::from_slice(&bytes) {
                    Ok(store) => store,
                    // 结构升级或文件被截断：索引只是缓存，救回元信息后静默重建，
                    // 不要把「解析失败」变成每次启动都弹出的告警。
                    Err(_) => {
                        let metadata: IndexMetadata =
                            serde_json::from_slice(&bytes).unwrap_or_default();
                        return Ok(Self {
                            version: INDEX_VERSION,
                            root: metadata.root,
                            enabled: metadata.enabled,
                            entries: BTreeMap::new(),
                        });
                    }
                };
                if store.version != INDEX_VERSION {
                    // 指纹或波形语义变化：保留「已启用」与根目录，只清空条目，
                    // 让索引在后台安静重建，不再要求用户重新确认。
                    store.entries.clear();
                }
                store.version = INDEX_VERSION;
                Ok(store)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(AudioIndexError::Read(error)),
        }
    }

    /// 索引是缓存：用紧凑 JSON 落盘，大曲库下体积与写入耗时都约为格式化 JSON 的 60%。
    pub fn save(&self, path: &Path) -> Result<(), AudioIndexError> {
        write_json_compact(path, self).map_err(AudioIndexError::Persist)
    }

    pub fn summaries(&self) -> Vec<AudioIndexSummary> {
        self.entries.values().map(IndexEntry::summary).collect()
    }

    /// 手动「更新索引」时清掉失败占位，让本轮立刻重试，而不是继续等退避窗口。
    pub fn retry_failed(&mut self) {
        self.entries.retain(|_, entry| entry.error.is_none());
    }

    pub fn info_for_song(&self, key: &str, path: &str) -> Option<AudioIndexInfo> {
        let normalized = normalize_path(Path::new(path));
        self.entries
            .get(&normalized)
            .or_else(|| {
                self.entries
                    .values()
                    .find(|entry| entry.key.as_deref() == Some(key) && entry.error.is_none())
            })
            .map(IndexEntry::info)
    }
}

pub fn normalize_path(path: &Path) -> String {
    strip_verbatim_prefix(&path.to_string_lossy()).replace('\\', "/")
}

/// 一次目录扫描的结果。
struct CandidateScan {
    candidates: Vec<IndexCandidate>,
    /// 成功读取（或确认不存在）的分类目录；只有这些分类的旧条目允许被删除，
    /// 网络盘临时掉线之类的读取错误不能清空索引。
    scanned: HashSet<String>,
    warnings: Vec<String>,
}

/// 扫描注册分类下的全部音频文件，包含曲库视图因同名多格式而忽略的副本。
fn collect_candidates(root: &Path, categories: &[Category], songs: &[Song]) -> CandidateScan {
    let song_by_path: HashMap<String, &Song> = songs
        .iter()
        .map(|song| (normalize_path(Path::new(&song.path)), song))
        .collect();
    let mut candidates = Vec::new();
    let mut scanned = HashSet::with_capacity(categories.len());
    let mut warnings = Vec::new();

    for category in categories {
        let dir = root.join(&category.id);
        if !dir.is_dir() {
            // 目录确实不在磁盘上：该分类为空，旧条目应当一并清理。
            scanned.insert(category.id.clone());
            continue;
        }
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) => {
                warnings.push(format!("索引扫描无法读取 {}：{error}", dir.display()));
                continue;
            }
        };
        scanned.insert(category.id.clone());
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !has_audio_extension(&file_name) {
                continue;
            }
            let metadata = match fs::metadata(&path) {
                Ok(metadata) if metadata.is_file() => metadata,
                _ => continue,
            };
            let id = normalize_path(&path);
            let song = song_by_path.get(&id).copied();
            let title = song
                .map(|song| song.title.clone())
                .or_else(|| {
                    path.file_stem()
                        .and_then(|stem| stem.to_str())
                        .map(|stem| stem.replace('_', " "))
                })
                .unwrap_or_else(|| file_name.clone());
            candidates.push(IndexCandidate {
                id,
                key: song.map(|song| song.key.clone()),
                title,
                file_name,
                category: category.id.clone(),
                size_bytes: metadata.len(),
                modified_ms: modified_ms(&metadata),
                duration_ms: song.and_then(|song| song.duration_ms),
            });
        }
    }
    candidates.sort_by(|left, right| left.id.cmp(&right.id));
    CandidateScan {
        candidates,
        scanned,
        warnings,
    }
}

/// 同步计划：只读计算，不触碰 ffmpeg，也不修改索引。
#[derive(Debug, Clone, Default)]
pub struct SyncPlan {
    root: String,
    root_changed: bool,
    /// 磁盘上仍然存在的条目 id。
    keep: HashSet<String>,
    /// 成功读取的分类目录。
    scanned: HashSet<String>,
    /// 需要重新解码的候选文件。
    todo: Vec<IndexCandidate>,
    /// 文件签名未变、直接沿用旧结果的曲目数。
    reused: usize,
    warnings: Vec<String>,
}

impl SyncPlan {
    /// 需要解码的文件数；`0` 表示本次同步是纯校验，无需通知前端。
    pub fn pending(&self) -> usize {
        self.todo.len()
    }
}

/// 单个候选文件的分析结果，`Err` 携带失败原因。
#[derive(Debug)]
pub struct AnalysisOutcome {
    candidate: IndexCandidate,
    result: Result<IndexEntry, String>,
}

/// 一批分析结果；`unavailable` 表示缺少 ffmpeg，本轮不应覆盖既有可用条目。
#[derive(Debug, Default)]
pub struct PlanAnalysis {
    results: Vec<AnalysisOutcome>,
    unavailable: bool,
}

impl AudioIndexStore {
    /// 计算增量同步计划：不持锁、不解码，毫秒级完成。
    pub fn plan_sync(&self, root: &Path, categories: &[Category], songs: &[Song]) -> SyncPlan {
        let normalized_root = normalize_path(root);
        let root_changed = self.root != normalized_root;
        let scan = collect_candidates(root, categories, songs);
        let now = now_ms();
        let mut keep = HashSet::with_capacity(scan.candidates.len());
        let mut todo = Vec::new();
        let mut reused = 0;

        for candidate in scan.candidates {
            keep.insert(candidate.id.clone());
            let existing = self.entries.get(&candidate.id);
            // 根目录变化或版本升级后，旧条目的指纹语义不再可信，一律重新分析。
            let matched =
                !root_changed && existing.is_some_and(|entry| entry.signature_matches(&candidate));
            // 同一份文件刚分析失败过：先做退避，等下一轮再试，别让兜底扫描反复解码。
            let backing_off = !root_changed
                && existing.is_some_and(|entry| {
                    entry.error.is_some()
                        && entry.same_source(&candidate)
                        && now.saturating_sub(entry.analyzed_at_ms) < FAILED_RETRY_MS
                });
            if matched || backing_off {
                reused += 1;
            } else {
                todo.push(candidate);
            }
        }

        SyncPlan {
            root: normalized_root,
            root_changed,
            keep,
            scanned: scan.scanned,
            todo,
            reused,
            warnings: scan.warnings,
        }
    }

    /// 把分析结果写回索引；返回报告与「是否有实际变化」。
    pub fn apply_sync(
        &mut self,
        plan: SyncPlan,
        analysis: PlanAnalysis,
    ) -> (IndexSyncReport, bool) {
        let mut report = IndexSyncReport {
            warnings: plan.warnings.clone(),
            reused: plan.reused,
            ..IndexSyncReport::default()
        };
        let mut changed = self.version != INDEX_VERSION || plan.root_changed;
        self.version = INDEX_VERSION;
        if plan.root_changed {
            if !self.entries.is_empty() {
                self.entries.clear();
            }
            self.root = plan.root.clone();
        }

        let before = self.entries.len();
        self.entries
            .retain(|id, entry| plan.keep.contains(id) || !plan.scanned.contains(&entry.category));
        report.removed = before - self.entries.len();
        changed |= report.removed > 0;

        if analysis.unavailable {
            // 没有 ffmpeg 时只做增删维护：既有指纹、BPM、波形全部保留，
            // 待 ffmpeg 可用后下一轮再补齐新增文件。
            report.failed = plan.todo.len();
            if !plan.todo.is_empty() {
                report
                    .warnings
                    .push("未找到可用的 ffmpeg，暂时无法建立音频索引".to_owned());
            }
            return (report, changed);
        }

        for outcome in analysis.results {
            let candidate = &outcome.candidate;
            match outcome.result {
                Ok(entry) => {
                    self.entries.insert(candidate.id.clone(), entry);
                    report.analyzed += 1;
                }
                Err(error) => {
                    report.failed += 1;
                    push_warning(
                        &mut report.warnings,
                        format!("{}：{error}", candidate.file_name),
                    );
                    self.entries
                        .insert(candidate.id.clone(), failed_entry(candidate, &error));
                }
            }
            changed = true;
        }

        (report, changed)
    }
}

/// 逐条告警超过上限后折叠，避免一次同步刷出上千条横幅。
fn push_warning(warnings: &mut Vec<String>, message: String) {
    if warnings.len() < MAX_REPORTED_WARNINGS {
        warnings.push(message);
    } else if warnings.len() == MAX_REPORTED_WARNINGS {
        warnings.push("……更多失败明细已省略".to_owned());
    }
}

/// 解码分析计划中的文件；不需要索引锁，可以在后台线程跑几分钟。
pub fn analyze_plan(
    plan: &SyncPlan,
    ffmpeg: Option<&Path>,
    mut progress: impl FnMut(usize, usize, &str),
) -> PlanAnalysis {
    analyze_plan_while(plan, ffmpeg, |current, total, title| {
        progress(current, total, title);
        true
    })
    .unwrap_or_default()
}

pub fn analyze_plan_while(
    plan: &SyncPlan,
    ffmpeg: Option<&Path>,
    mut progress: impl FnMut(usize, usize, &str) -> bool,
) -> Option<PlanAnalysis> {
    let total = plan.todo.len();
    if total == 0 {
        return Some(PlanAnalysis::default());
    }
    let Some(ffmpeg) = ffmpeg else {
        return Some(PlanAnalysis {
            results: Vec::new(),
            unavailable: true,
        });
    };
    let mut results = Vec::with_capacity(total);
    for (index, candidate) in plan.todo.iter().enumerate() {
        if !progress(index + 1, total, &candidate.title) {
            return None;
        }
        results.push(AnalysisOutcome {
            candidate: candidate.clone(),
            result: analyze_file(ffmpeg, candidate).map_err(|error| error.to_string()),
        });
    }
    Some(PlanAnalysis {
        results,
        unavailable: false,
    })
}

/// 在当前线程一次性完成计划、分析与写回；供单线程调用方与测试使用。
pub fn sync_index(
    store: &mut AudioIndexStore,
    path: &Path,
    root: &Path,
    categories: &[Category],
    songs: &[Song],
    ffmpeg: Option<&Path>,
    progress: impl FnMut(usize, usize, &str),
) -> Result<IndexSyncReport, AudioIndexError> {
    let plan = store.plan_sync(root, categories, songs);
    let analysis = analyze_plan(&plan, ffmpeg, progress);
    let (mut report, changed) = store.apply_sync(plan, analysis);
    let changed = changed || !store.enabled;
    store.enabled = true;
    if changed {
        store.save(path)?;
    }
    report.changed = changed;
    Ok(report)
}

fn failed_entry(candidate: &IndexCandidate, error: &str) -> IndexEntry {
    IndexEntry {
        id: candidate.id.clone(),
        key: candidate.key.clone(),
        title: candidate.title.clone(),
        file_name: candidate.file_name.clone(),
        category: candidate.category.clone(),
        path: candidate.id.clone(),
        size_bytes: candidate.size_bytes,
        modified_ms: candidate.modified_ms,
        duration_ms: candidate.duration_ms,
        bpm: None,
        musical_key: None,
        integrated_lufs: None,
        peak_db: None,
        true_peak_db: None,
        waveform: Vec::new(),
        fingerprint: Vec::new(),
        analyzed_at_ms: now_ms(),
        error: Some(error.to_owned()),
    }
}

fn analyze_file(ffmpeg: &Path, candidate: &IndexCandidate) -> Result<IndexEntry, AudioIndexError> {
    let source = Path::new(&candidate.id);
    // 指纹与 PCM 是两条独立的解码链，并行跑能把单曲分析时间压掉近一半。
    let (fingerprint, decoded) = std::thread::scope(|scope| {
        let fingerprint = scope.spawn(|| chromaprint(ffmpeg, source));
        let decoded = scope.spawn(|| decode_mono(ffmpeg, source));
        (
            fingerprint.join().unwrap_or_else(|_| {
                Err(AudioIndexError::Analyze("指纹分析线程异常结束".to_owned()))
            }),
            decoded.join().unwrap_or_else(|_| {
                Err(AudioIndexError::Analyze("音频解码线程异常结束".to_owned()))
            }),
        )
    });
    let fingerprint = fingerprint?;
    let (samples, integrated_lufs, true_peak_db) = decoded?;
    let peak = samples
        .iter()
        .fold(0.0_f64, |peak, sample| peak.max(sample.abs() as f64));
    let waveform = compute_waveform(&samples, WAVEFORM_BUCKETS);
    let bpm = estimate_bpm(&samples, ANALYSIS_SAMPLE_RATE);
    let musical_key = estimate_key(&samples, ANALYSIS_SAMPLE_RATE);

    Ok(IndexEntry {
        id: candidate.id.clone(),
        key: candidate.key.clone(),
        title: candidate.title.clone(),
        file_name: candidate.file_name.clone(),
        category: candidate.category.clone(),
        path: candidate.id.clone(),
        size_bytes: candidate.size_bytes,
        modified_ms: candidate.modified_ms,
        duration_ms: candidate.duration_ms,
        bpm,
        musical_key,
        integrated_lufs,
        peak_db: (peak > 0.0).then(|| 20.0 * peak.log10()),
        true_peak_db,
        waveform,
        fingerprint,
        analyzed_at_ms: now_ms(),
        error: None,
    })
}

/// 读取 Chromaprint 子指纹序列。
///
/// 必须显式指定 `-fp_format raw`：muxer 默认输出的是「压缩后再 Base64」的比特流，
/// 按 4 字节定长切分会把值边界切碎，同一段音频换个起点就再也对不上，
/// 以声搜声与「相似重复」会整体失效。raw 格式是逐个 4 字节小端 u32，可直接对齐。
fn chromaprint(ffmpeg: &Path, source: &Path) -> Result<Vec<u32>, AudioIndexError> {
    let output = command(ffmpeg)
        .args([
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-protocol_whitelist",
            "file",
            "-i",
        ])
        .arg(source)
        .args([
            "-vn",
            "-ac",
            "1",
            "-ar",
            "11025",
            "-fp_format",
            "raw",
            "-f",
            "chromaprint",
            "pipe:1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| AudioIndexError::Analyze(format!("无法启动 ffmpeg：{error}")))?;
    if !output.status.success() {
        return Err(AudioIndexError::Analyze(format!(
            "ffmpeg 指纹分析失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    if output.stdout.len() % 4 != 0 {
        return Err(AudioIndexError::Analyze(
            "ffmpeg 返回了不完整的音频指纹".to_owned(),
        ));
    }
    let fingerprint: Vec<u32> = output
        .stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| u32::from_le_bytes(*chunk))
        .collect();
    if fingerprint.is_empty() {
        return Err(AudioIndexError::Analyze(
            "ffmpeg 未返回有效的音频指纹".to_owned(),
        ));
    }
    Ok(fingerprint)
}

/// 解码结果：单声道采样、综合响度、真峰值。
type DecodedAudio = (Vec<f32>, Option<f64>, Option<f64>);

fn decode_mono(ffmpeg: &Path, source: &Path) -> Result<DecodedAudio, AudioIndexError> {
    let output = command(ffmpeg)
        .args(["-hide_banner", "-nostdin", "-loglevel", "info", "-i"])
        .arg(source)
        .args([
            "-vn",
            "-af",
            "aresample=11025,aformat=sample_fmts=fltp:channel_layouts=mono,ebur128=peak=true:framelog=verbose",
            "-f",
            "f32le",
            "pipe:1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| AudioIndexError::Analyze(format!("无法启动 ffmpeg：{error}")))?;
    if !output.status.success() {
        return Err(AudioIndexError::Analyze(format!(
            "ffmpeg 音频解码失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    if output.stdout.len() % 4 != 0 {
        return Err(AudioIndexError::Analyze(
            "ffmpeg 返回了不完整的 PCM 数据".to_owned(),
        ));
    }
    let samples: Vec<f32> = output
        .stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect();
    if samples.is_empty() {
        return Err(AudioIndexError::Analyze("音频没有可分析的采样".to_owned()));
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let (integrated_lufs, true_peak_db) = parse_ebur128(&stderr);
    Ok((samples, integrated_lufs, true_peak_db))
}

fn parse_ebur128(stderr: &str) -> (Option<f64>, Option<f64>) {
    let Some(summary) = stderr.rsplit_once("Summary:").map(|(_, tail)| tail) else {
        return (None, None);
    };
    let mut integrated = None;
    let mut peak = None;
    for line in summary.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("I:") {
            integrated = value
                .split_whitespace()
                .next()
                .and_then(|value| value.parse().ok());
        } else if let Some(value) = line.strip_prefix("Peak:") {
            peak = value
                .split_whitespace()
                .next()
                .and_then(|value| value.parse().ok());
        }
    }
    (integrated, peak)
}

fn compute_waveform(samples: &[f32], buckets: usize) -> Vec<f32> {
    let mut peaks = vec![0.0_f32; buckets];
    if samples.is_empty() {
        return peaks;
    }
    for (bucket, peak) in peaks.iter_mut().enumerate() {
        let start = bucket * samples.len() / buckets;
        let end = ((bucket + 1) * samples.len() / buckets).max(start + 1);
        let mut maximum = 0.0_f32;
        for sample in &samples[start..end.min(samples.len())] {
            maximum = maximum.max(sample.abs());
        }
        // 量化由落盘编码负责（见 [`codec::waveform`]），内存里保留原始峰值。
        *peak = maximum.min(1.0);
    }
    peaks
}

fn estimate_bpm(samples: &[f32], sample_rate: u32) -> Option<f64> {
    let frame = 1024;
    let hop = 256;
    let limit = samples.len().min(sample_rate as usize * 180);
    if limit < frame * 4 {
        return None;
    }
    let mut energy = Vec::with_capacity(limit / hop);
    for start in (0..limit - frame).step_by(hop) {
        let sum: f64 = samples[start..start + frame]
            .iter()
            .map(|sample| (*sample as f64).powi(2))
            .sum();
        energy.push((sum / frame as f64 + 1e-12).log10());
    }
    let mut onset: Vec<f64> = energy
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).max(0.0))
        .collect();
    let mean = onset.iter().sum::<f64>() / onset.len() as f64;
    for value in &mut onset {
        *value = (*value - mean).max(0.0);
    }
    if onset.iter().sum::<f64>() < 1e-6 {
        return None;
    }

    let min_lag = (60.0 * sample_rate as f64 / (180.0 * hop as f64)).floor() as usize;
    let max_lag = (60.0 * sample_rate as f64 / (60.0 * hop as f64)).ceil() as usize;
    let mut best = (f64::NEG_INFINITY, min_lag.max(1));
    for lag in min_lag.max(1)..=max_lag.min(onset.len() / 2) {
        let mut correlation = 0.0;
        for index in 0..onset.len() - lag {
            correlation += onset[index] * onset[index + lag];
        }
        let bpm = 60.0 * sample_rate as f64 / (lag * hop) as f64;
        let tempo_weight = 1.0 / (1.0 + (bpm / 120.0).ln().abs() * 0.35);
        let score = correlation * tempo_weight;
        if score > best.0 {
            best = (score, lag);
        }
    }
    let bpm = 60.0 * sample_rate as f64 / (best.1 * hop) as f64;
    (bpm.is_finite() && (55.0..=185.0).contains(&bpm)).then_some(bpm)
}

fn estimate_key(samples: &[f32], sample_rate: u32) -> Option<String> {
    const SIZE: usize = 2048;
    const HOP: usize = 1024;
    let limit = samples.len().min(sample_rate as usize * 120);
    if limit < SIZE * 4 {
        return None;
    }
    let window: Vec<f64> = (0..SIZE)
        .map(|index| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * index as f64 / SIZE as f64).cos())
        .collect();
    let mut chroma = [0.0_f64; 12];
    let mut frames = 0;
    for start in (0..limit - SIZE).step_by(HOP) {
        let mut real = vec![0.0_f64; SIZE];
        let mut imaginary = vec![0.0_f64; SIZE];
        for index in 0..SIZE {
            real[index] = samples[start + index] as f64 * window[index];
        }
        fft(&mut real, &mut imaginary);
        for bin in 1..SIZE / 2 {
            let frequency = bin as f64 * sample_rate as f64 / SIZE as f64;
            if !(65.0..=2000.0).contains(&frequency) {
                continue;
            }
            let magnitude = (real[bin] * real[bin] + imaginary[bin] * imaginary[bin]).sqrt();
            let midi = 69.0 + 12.0 * (frequency / 440.0).log2();
            let pitch_class = (midi.round() as i32).rem_euclid(12) as usize;
            chroma[pitch_class] += magnitude;
        }
        frames += 1;
    }
    if frames == 0 || chroma.iter().sum::<f64>() < 1e-8 {
        return None;
    }

    const MAJOR: [f64; 12] = [
        6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
    ];
    const MINOR: [f64; 12] = [
        6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
    ];
    let mut best = (f64::NEG_INFINITY, 0, false);
    for root in 0..12 {
        let major: f64 = (0..12)
            .map(|index| chroma[index] * MAJOR[(index + 12 - root) % 12])
            .sum();
        let minor: f64 = (0..12)
            .map(|index| chroma[index] * MINOR[(index + 12 - root) % 12])
            .sum();
        if major > best.0 {
            best = (major, root, false);
        }
        if minor > best.0 {
            best = (minor, root, true);
        }
    }
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    Some(format!(
        "{}{}",
        NAMES[best.1],
        if best.2 { "m" } else { "" }
    ))
}

fn fft(real: &mut [f64], imaginary: &mut [f64]) {
    let length = real.len();
    let mut reversed = 0;
    for index in 1..length {
        let mut bit = length >> 1;
        while reversed & bit != 0 {
            reversed ^= bit;
            bit >>= 1;
        }
        reversed ^= bit;
        if index < reversed {
            real.swap(index, reversed);
            imaginary.swap(index, reversed);
        }
    }
    let mut size = 2;
    while size <= length {
        let angle = -2.0 * std::f64::consts::PI / size as f64;
        let root_real = angle.cos();
        let root_imaginary = angle.sin();
        for start in (0..length).step_by(size) {
            let mut phase_real = 1.0;
            let mut phase_imaginary = 0.0;
            for offset in 0..size / 2 {
                let even = start + offset;
                let odd = even + size / 2;
                let odd_real = real[odd] * phase_real - imaginary[odd] * phase_imaginary;
                let odd_imaginary = real[odd] * phase_imaginary + imaginary[odd] * phase_real;
                real[odd] = real[even] - odd_real;
                imaginary[odd] = imaginary[even] - odd_imaginary;
                real[even] += odd_real;
                imaginary[even] += odd_imaginary;
                let next_real = phase_real * root_real - phase_imaginary * root_imaginary;
                phase_imaginary = phase_real * root_imaginary + phase_imaginary * root_real;
                phase_real = next_real;
            }
        }
        size <<= 1;
    }
}

fn duplicate_track(entry: &IndexEntry) -> DuplicateTrack {
    DuplicateTrack {
        id: entry.id.clone(),
        key: entry.key.clone(),
        title: entry.title.clone(),
        file_name: entry.file_name.clone(),
        category: entry.category.clone(),
        path: entry.path.clone(),
        size_bytes: entry.size_bytes,
        duration_ms: entry.duration_ms,
        bpm: entry.bpm,
        musical_key: entry.musical_key.clone(),
    }
}

type Postings = HashMap<u32, Vec<(usize, u32)>>;

fn build_postings(entries: &[&IndexEntry]) -> Postings {
    let mut postings: Postings = HashMap::new();
    for (track_index, entry) in entries.iter().enumerate() {
        for (frame, word) in entry.fingerprint.iter().enumerate() {
            if *word == 0 {
                continue;
            }
            postings
                .entry(*word)
                .or_default()
                .push((track_index, frame as u32));
        }
    }
    postings
}

fn consolidate_offsets(votes: &HashMap<i32, usize>) -> (usize, i32, usize) {
    let mut best = (0, 0_i32, 0_usize);
    for (&offset, &count) in votes {
        let window: usize = (-1..=1)
            .map(|delta| votes.get(&(offset + delta)).copied().unwrap_or(0))
            .sum();
        if window > best.0 || (window == best.0 && offset.abs() < best.1.abs()) {
            best = (window, offset, count);
        }
    }
    best
}

pub fn find_duplicates(entries: &[&IndexEntry], threshold: f64) -> DuplicateReport {
    let usable: Vec<&IndexEntry> = entries
        .iter()
        .copied()
        .filter(|entry| entry.error.is_none() && !entry.fingerprint.is_empty())
        .collect();
    let failed_tracks = entries.len() - usable.len();
    let postings = build_postings(&usable);
    let mut parents: Vec<usize> = (0..usable.len()).collect();
    let mut scores: HashMap<(usize, usize), f64> = HashMap::new();

    for right in 0..usable.len() {
        let mut votes: HashMap<(usize, i32), usize> = HashMap::new();
        for (query_frame, word) in usable[right].fingerprint.iter().enumerate() {
            let Some(occurrences) = postings.get(word) else {
                continue;
            };
            if occurrences.len() > MAX_COMMON_POSTINGS {
                continue;
            }
            for &(left, candidate_frame) in occurrences {
                if left >= right {
                    continue;
                }
                *votes
                    .entry((left, candidate_frame as i32 - query_frame as i32))
                    .or_default() += 1;
            }
        }
        let mut by_left: HashMap<usize, HashMap<i32, usize>> = HashMap::new();
        for ((left, offset), count) in votes {
            by_left.entry(left).or_default().insert(offset, count);
        }
        for (left, offsets) in by_left {
            let (matched, _, _) = consolidate_offsets(&offsets);
            let coverage = matched as f64
                / usable[left]
                    .fingerprint
                    .len()
                    .min(usable[right].fingerprint.len())
                    .max(1) as f64;
            if matched >= 12 && coverage >= threshold {
                scores.insert((left, right), coverage);
                union(&mut parents, left, right);
            }
        }
    }

    let mut grouped: HashMap<usize, Vec<usize>> = HashMap::new();
    for index in 0..usable.len() {
        grouped
            .entry(find_root(&mut parents, index))
            .or_default()
            .push(index);
    }
    let mut groups: Vec<DuplicateGroup> = grouped
        .into_values()
        .filter(|indices| indices.len() >= 2)
        .map(|mut indices| {
            indices.sort_by(|left, right| {
                usable[*left]
                    .key
                    .is_none()
                    .cmp(&usable[*right].key.is_none())
                    .then_with(|| usable[*left].id.cmp(&usable[*right].id))
            });
            let score = indices
                .iter()
                .map(|&right| {
                    indices
                        .iter()
                        .filter(|&&left| left < right)
                        .map(|&left| *scores.get(&(left, right)).unwrap_or(&threshold))
                        .fold(f64::INFINITY, f64::min)
                })
                .fold(f64::INFINITY, f64::min);
            let exact = indices
                .iter()
                .all(|&index| usable[index].fingerprint == usable[indices[0]].fingerprint);
            DuplicateGroup {
                id: format!("dup-{}", usable[indices[0]].id),
                match_kind: if exact { "exact" } else { "similar" },
                score: if score.is_finite() { score } else { threshold },
                tracks: indices
                    .iter()
                    .map(|&index| duplicate_track(usable[index]))
                    .collect(),
            }
        })
        .collect();
    groups.sort_by(|left, right| left.id.cmp(&right.id));
    DuplicateReport {
        groups,
        indexed_tracks: usable.len(),
        failed_tracks,
    }
}

pub fn search_by_fingerprint(
    entries: &[&IndexEntry],
    query: &[u32],
    query_path: &str,
    limit: usize,
    threshold: f64,
) -> SoundSearchResult {
    let usable: Vec<&IndexEntry> = entries
        .iter()
        .copied()
        .filter(|entry| entry.error.is_none() && !entry.fingerprint.is_empty())
        .collect();
    let postings = build_postings(&usable);
    let normalized_query = normalize_path(Path::new(query_path));
    let mut votes: Vec<HashMap<i32, usize>> = vec![HashMap::new(); usable.len()];
    // Bound fuzzy lookups for long recordings; candidate alignments still use every frame.
    let stride = query.len().div_ceil(256).max(1);
    for (query_frame, &word) in query.iter().enumerate().step_by(stride) {
        if word == 0 {
            continue;
        }
        let mut vote = |nearby_word| {
            let Some(occurrences) = postings.get(&nearby_word) else {
                return;
            };
            if occurrences.len() > MAX_COMMON_POSTINGS {
                return;
            }
            for &(track_index, candidate_frame) in occurrences {
                *votes[track_index]
                    .entry(candidate_frame as i32 - query_frame as i32)
                    .or_default() += 1;
            }
        };
        vote(word);
        for bit in 0..32 {
            let one_flip = word ^ (1 << bit);
            vote(one_flip);
            for second_bit in 0..bit {
                vote(one_flip ^ (1 << second_bit));
            }
        }
    }
    let mut matches: Vec<SoundMatch> = votes
        .into_iter()
        .enumerate()
        .filter_map(|(index, offsets)| {
            let entry = usable[index];
            if entry.id == normalized_query {
                return None;
            }
            let mut offsets: Vec<_> = offsets.into_iter().collect();
            offsets.sort_unstable_by_key(|&(offset, votes)| {
                (std::cmp::Reverse(votes), offset.abs(), offset)
            });
            let (matched, exact, offset) = offsets
                .into_iter()
                .take(8)
                .map(|(offset, _)| {
                    let query_start = (-offset).max(0) as usize;
                    let entry_start = offset.max(0) as usize;
                    let (matched, exact) = query[query_start..]
                        .iter()
                        .zip(&entry.fingerprint[entry_start..])
                        .filter(|(left, right)| **left != 0 && **right != 0)
                        .map(|(left, right)| (left ^ right).count_ones())
                        .fold((0, 0), |(matched, exact), difference| {
                            (
                                matched + usize::from(difference <= 6),
                                exact + usize::from(difference == 0),
                            )
                        });
                    (matched, exact, offset)
                })
                .max_by_key(|&(matched, exact, offset)| {
                    (matched, exact, std::cmp::Reverse(offset.abs()))
                })?;
            // 近似命中略低于精确命中，避免翻录片段挤掉原声结果。
            let denominator = query.len().min(entry.fingerprint.len()).max(1);
            let score = (0.85 * matched as f64 + 0.15 * exact as f64) / denominator as f64;
            (matched >= 8 && score >= threshold).then(|| SoundMatch {
                track: duplicate_track(entry),
                score,
                offset_seconds: offset as f64 * FINGERPRINT_FRAME_SECONDS,
                matched_frames: matched,
            })
        })
        .collect();
    matches.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.track.id.cmp(&right.track.id))
    });
    matches.truncate(limit);
    SoundSearchResult {
        query_path: query_path.to_owned(),
        matches,
    }
}

pub fn analyze_query_fingerprint(ffmpeg: &Path, path: &Path) -> Result<Vec<u32>, AudioIndexError> {
    chromaprint(ffmpeg, path)
}

fn union(parents: &mut [usize], left: usize, right: usize) {
    let left = find_root(parents, left);
    let right = find_root(parents, right);
    if left != right {
        parents[right] = left;
    }
}

fn find_root(parents: &mut [usize], mut index: usize) -> usize {
    while parents[index] != index {
        parents[index] = parents[parents[index]];
        index = parents[index];
    }
    index
}

fn modified_ms(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, fingerprint: Vec<u32>) -> IndexEntry {
        IndexEntry {
            id: id.to_owned(),
            key: Some(id.to_owned()),
            title: id.to_owned(),
            file_name: format!("{id}.mp3"),
            category: "epic".into(),
            path: format!("D:/bgm/epic/{id}.mp3"),
            size_bytes: 1,
            modified_ms: 1,
            duration_ms: Some(60_000),
            bpm: None,
            musical_key: None,
            integrated_lufs: None,
            peak_db: None,
            true_peak_db: None,
            waveform: Vec::new(),
            fingerprint,
            analyzed_at_ms: 1,
            error: None,
        }
    }

    #[test]
    fn parses_only_the_final_ebur128_summary() {
        let stderr = "I: -99.0 LUFS\nSummary:\n  Integrated loudness:\n    I: -14.2 LUFS\n  True peak:\n    Peak: -1.3 dBFS\n";
        let (integrated, peak) = parse_ebur128(stderr);
        assert_eq!(integrated, Some(-14.2));
        assert_eq!(peak, Some(-1.3));
    }

    #[test]
    fn groups_matching_fingerprints() {
        let first = entry("a", (1..40).collect());
        let second = entry("b", (1..40).collect());
        let other = entry("c", (100..140).collect());
        let refs = [&first, &second, &other];
        let report = find_duplicates(&refs, 0.85);
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].tracks.len(), 2);
        assert_eq!(report.groups[0].match_kind, "exact");
    }

    #[test]
    fn finds_a_query_inside_a_longer_track() {
        let query: Vec<u32> = (100..140).collect();
        let candidate = entry("long", (1..160).collect());
        let refs = [&candidate];
        let result = search_by_fingerprint(&refs, &query, "D:/query.wav", 10, 0.8);
        assert_eq!(result.matches.len(), 1);
        assert!(result.matches[0].score > 0.95);
        assert_eq!(
            result.matches[0].offset_seconds,
            99.0 * FINGERPRINT_FRAME_SECONDS
        );
    }

    #[test]
    fn ranks_exact_matches_first_and_limits_noisy_clip_results() {
        let query: Vec<_> = (1_u32..80).map(|i| i.wrapping_mul(0x9e37_79b9)).collect();
        let noisy: Vec<_> = query.iter().map(|frame| frame ^ 0b11).collect();
        let exact = entry("exact", query.clone());
        let recorded = entry("recorded", noisy);
        let unrelated = entry("unrelated", vec![1; 80]);
        let tracks = [&unrelated, &recorded, &exact];
        let result = search_by_fingerprint(&tracks, &query, "clip.webm", 2, 0.35);
        assert_eq!(result.matches.len(), 2);
        assert_eq!(result.matches[0].track.id, "exact");
        assert_eq!(result.matches[1].track.id, "recorded");
        let limited = search_by_fingerprint(&tracks, &query, "clip.webm", 1, 0.35);
        assert_eq!(limited.matches.len(), 1);
    }

    #[test]
    fn groups_a_longer_track_regardless_of_fingerprint_order() {
        let short = entry("short", (100..140).collect());
        let long = entry("long", (1..160).collect());
        for refs in [&[&short, &long][..], &[&long, &short][..]] {
            let report = find_duplicates(refs, 0.85);
            assert_eq!(report.groups.len(), 1);
            assert_eq!(report.groups[0].match_kind, "similar");
        }
    }

    #[test]
    fn waveform_has_fixed_bucket_count() {
        let samples = [0.0_f32, 0.25, -0.5, 1.0];
        let waveform = compute_waveform(&samples, 4);
        assert_eq!(waveform.len(), 4);
        assert_eq!(waveform[3], 1.0);
    }

    #[test]
    fn failed_analysis_backs_off_between_syncs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("library");
        fs::create_dir_all(root.join("epic")).unwrap();
        let tracked = root.join("epic/broken.wav");
        fs::write(&tracked, b"not really audio").unwrap();
        let categories = [Category {
            id: "epic".into(),
            ..Category::default()
        }];
        let index_path = dir.path().join("audio-index.json");
        // 指向不存在的可执行文件，稳定复现「分析失败」。
        let bogus = Path::new("tfy-bgm-missing-ffmpeg");
        let mut store = AudioIndexStore::default();

        let first = sync_index(
            &mut store,
            &index_path,
            &root,
            &categories,
            &[],
            Some(bogus),
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(first.failed, 1);
        assert!(store.entries.values().next().unwrap().error.is_some());

        let second = sync_index(
            &mut store,
            &index_path,
            &root,
            &categories,
            &[],
            Some(bogus),
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(second.failed, 0, "退避窗口内不应重复解码同一份坏文件");
        assert_eq!(second.reused, 1);

        // 文件内容变化后必须立刻重试，而不是等退避结束。
        fs::write(&tracked, b"changed content").unwrap();
        let third = sync_index(
            &mut store,
            &index_path,
            &root,
            &categories,
            &[],
            Some(bogus),
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(third.failed, 1);
    }

    #[test]
    fn index_round_trips_fingerprint_and_waveform_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audio-index.json");
        let mut store = AudioIndexStore {
            version: INDEX_VERSION,
            root: "D:/bgm".into(),
            enabled: true,
            entries: BTreeMap::new(),
        };
        let mut sample = entry("a", vec![0, 1, 0xDEAD_BEEF, u32::MAX, 42]);
        sample.waveform = vec![0.0, 0.25, 0.5, 1.0];
        store.entries.insert("a".into(), sample);
        store.save(&path).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("3735928559"),
            "指纹应以 Base64 落盘，而不是十进制数字：{text}"
        );
        let restored = AudioIndexStore::load(&path).unwrap();
        let restored = restored.entries.get("a").unwrap();
        // 指纹必须逐位还原，否则去重与检索都会被悄悄改变。
        assert_eq!(restored.fingerprint, vec![0, 1, 0xDEAD_BEEF, u32::MAX, 42]);
        assert_eq!(restored.waveform.len(), 4);
        assert!((restored.waveform[2] - 0.5).abs() < 0.01);
        assert_eq!(restored.waveform[3], 1.0);
    }

    #[test]
    fn unreadable_cache_keeps_enabled_flag_and_drops_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audio-index.json");
        // 旧版本的指纹是十进制数组，新结构无法解析；此时只保留启用状态与根目录。
        fs::write(
            &path,
            br#"{"version":1,"root":"D:/bgm","enabled":true,"entries":{"a":{"id":"a","fingerprint":[1,2]}}}"#,
        )
        .unwrap();
        let store = AudioIndexStore::load(&path).unwrap();
        assert!(store.enabled);
        assert_eq!(store.root, "D:/bgm");
        assert!(store.entries.is_empty());
        assert_eq!(store.version, INDEX_VERSION);
    }

    #[test]
    fn analyzes_a_real_audio_file_when_ffmpeg_is_available() {
        let Some(ffmpeg) = crate::loudness::ffmpeg_path() else {
            return;
        };
        let has_chromaprint = command(&ffmpeg)
            .args(["-hide_banner", "-muxers"])
            .output()
            .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains("chromaprint"));
        if !has_chromaprint {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("tone.wav");
        let output = command(&ffmpeg)
            .args([
                "-hide_banner",
                "-nostdin",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
            ])
            .arg("sine=frequency=440:duration=12")
            .arg(&source)
            .output()
            .unwrap();
        assert!(output.status.success());
        let fingerprint = analyze_query_fingerprint(&ffmpeg, &source).unwrap();
        assert!(!fingerprint.is_empty());
        let (samples, integrated, true_peak) = decode_mono(&ffmpeg, &source).unwrap();
        assert!(!samples.is_empty());
        assert!(integrated.is_some());
        assert!(true_peak.is_some());
        assert_eq!(compute_waveform(&samples, 800).len(), 800);

        // 纯正弦波的 Chromaprint 可能全为零；用粉红噪声验证真实指纹去重与检索。
        let music = dir.path().join("music.wav");
        let generated = command(&ffmpeg)
            .args([
                "-hide_banner",
                "-nostdin",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
            ])
            .arg("anoisesrc=color=pink:duration=12")
            .arg(&music)
            .output()
            .unwrap();
        assert!(generated.status.success());

        let root = dir.path().join("library");
        fs::create_dir_all(root.join("epic")).unwrap();
        let tracked = root.join("epic/tone.wav");
        fs::copy(&music, &tracked).unwrap();
        let categories = [Category {
            id: "epic".into(),
            ..Category::default()
        }];
        let index_path = dir.path().join("audio-index.json");
        let mut store = AudioIndexStore::default();
        let first = sync_index(
            &mut store,
            &index_path,
            &root,
            &categories,
            &[],
            Some(&ffmpeg),
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(first.analyzed, 1);
        assert!(store.enabled);
        let copy = root.join("epic/copy.wav");
        fs::copy(&music, &copy).unwrap();
        let second = sync_index(
            &mut store,
            &index_path,
            &root,
            &categories,
            &[],
            Some(&ffmpeg),
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(second.reused, 1);
        assert_eq!(second.analyzed, 1);
        let entries: Vec<_> = store.entries.values().collect();
        let duplicates = find_duplicates(&entries, 0.85);
        assert_eq!(duplicates.groups.len(), 1);
        assert_eq!(duplicates.groups[0].match_kind, "exact");
        let matches = search_by_fingerprint(
            &entries,
            &entries[0].fingerprint,
            &music.to_string_lossy(),
            10,
            0.8,
        );
        assert_eq!(matches.matches.len(), 2);
        assert!(matches.matches.iter().all(|matched| matched.score > 0.99));

        // 真实音频的片段检索：截取 6 秒片段回搜整曲，必须命中且偏移正确。
        // 若指纹退回 ffmpeg 默认的 Base64 压缩流，这里会一条都命中不了。
        let segment = dir.path().join("segment.wav");
        let clip = command(&ffmpeg)
            .args([
                "-hide_banner",
                "-nostdin",
                "-loglevel",
                "error",
                "-y",
                "-ss",
            ])
            .arg("6")
            .arg("-i")
            .arg(&music)
            .args(["-t", "6", "-ac", "1"])
            .arg(&segment)
            .output()
            .unwrap();
        assert!(clip.status.success());
        let clip_fingerprint = analyze_query_fingerprint(&ffmpeg, &segment).unwrap();
        let found = search_by_fingerprint(
            &entries,
            &clip_fingerprint,
            &segment.to_string_lossy(),
            10,
            0.2,
        );
        assert_eq!(found.matches.len(), 2, "片段应命中两个副本");
        for matched in &found.matches {
            assert!(
                (matched.offset_seconds - 6.0).abs() < 1.0,
                "片段偏移应接近 6 秒，实际 {}",
                matched.offset_seconds
            );
        }

        fs::remove_file(&tracked).unwrap();
        fs::remove_file(copy).unwrap();
        let third = sync_index(
            &mut store,
            &index_path,
            &root,
            &categories,
            &[],
            Some(&ffmpeg),
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(third.removed, 2);
        assert!(store.entries.is_empty());
        assert!(
            AudioIndexStore::load(&index_path)
                .unwrap()
                .entries
                .is_empty()
        );

        // 没有 ffmpeg 时仍须维护删除，但不能把已分析好的结果换成失败占位。
        fs::copy(&music, &tracked).unwrap();
        let rebuilt = sync_index(
            &mut store,
            &index_path,
            &root,
            &categories,
            &[],
            Some(&ffmpeg),
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(rebuilt.analyzed, 1);
        let mut bytes = fs::read(&tracked).unwrap();
        bytes.extend_from_slice(&[0, 0]);
        fs::write(&tracked, bytes).unwrap();
        let unavailable = sync_index(
            &mut store,
            &index_path,
            &root,
            &categories,
            &[],
            None,
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(unavailable.failed, 1);
        assert!(!unavailable.changed, "无 ffmpeg 且无增删时不应写盘");
        let kept = store
            .entries
            .get(&normalize_path(&tracked))
            .expect("既有条目必须保留");
        assert!(kept.error.is_none(), "缺少 ffmpeg 不能覆盖可用指纹");
        assert!(!kept.fingerprint.is_empty());

        fs::remove_file(&tracked).unwrap();
        let removed_without_ffmpeg = sync_index(
            &mut store,
            &index_path,
            &root,
            &categories,
            &[],
            None,
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(removed_without_ffmpeg.removed, 1);
        assert!(store.entries.is_empty());
    }

    #[test]
    fn unchanged_library_does_not_rewrite_the_index_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("library");
        fs::create_dir_all(root.join("epic")).unwrap();
        let path = dir.path().join("audio-index.json");
        let categories = [Category {
            id: "epic".into(),
            ..Category::default()
        }];
        let mut store = AudioIndexStore::default();
        let plan = store.plan_sync(&root, &categories, &[]);
        assert!(plan.pending() == 0);
        let (report, changed) = store.apply_sync(plan, PlanAnalysis::default());
        assert!(changed, "首次同步需要落地根目录");
        assert_eq!(report.removed, 0);
        store.save(&path).unwrap();

        let mut second = AudioIndexStore::load(&path).unwrap();
        let plan = second.plan_sync(&root, &categories, &[]);
        let (report, changed) = second.apply_sync(plan, PlanAnalysis::default());
        assert!(!changed, "空曲库的重复同步不产生任何变化");
        assert_eq!(report.analyzed + report.removed + report.failed, 0);
    }
}
