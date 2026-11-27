//! 本地指纹索引、片段匹配和重复文件候选。

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::BufReader;
use std::num::{NonZeroU16, NonZeroU32};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::SystemTime;

use rodio::{Decoder, buffer::SamplesBuffer, source::UniformSourceIterator};
use serde::{Deserialize, Serialize};

const VERSION: u8 = 2;
const RATE: u32 = 11_025;
const WINDOW: usize = 2048;
const HOP: usize = 1024;
const BANDS: usize = 17;
const MAX_FRAMES: usize = RATE as usize * 30 * 60 / HOP;
const QUERY_FRAMES: usize = RATE as usize * 30 / HOP;
const MIN_HITS: usize = 12;

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    pub limit: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            limit: 10,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedRoot {
    pub folder_id: String,
    pub path: PathBuf,
    pub legacy_key: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FileId {
    #[serde(default)]
    pub folder_id: String,
    pub category: String,
    pub name: String,
    #[serde(default)]
    pub song_key: String,
}

impl FileId {
    pub fn key(&self) -> String {
        if !self.song_key.is_empty() {
            return self.song_key.clone();
        }
        let stem = Path::new(&self.name)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or(&self.name);
        format!("{}/{stem}", self.category)
    }

    pub fn path(&self, root: &Path) -> PathBuf {
        if self.category == crate::library::UNCATEGORIZED_CATEGORY_ID {
            root.join(&self.name)
        } else {
            root.join(&self.category).join(&self.name)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileVersion {
    bytes: u64,
    created: Option<SystemTime>,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    inode: u64,
}

impl FileVersion {
    pub fn read(path: &Path) -> Option<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        let meta = fs::symlink_metadata(path)
            .ok()
            .filter(|meta| meta.is_file())?;
        Some(Self {
            bytes: meta.len(),
            created: meta.created().ok(),
            modified: meta.modified().ok(),
            #[cfg(unix)]
            inode: meta.ino(),
        })
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Item {
    file: FileId,
    version: FileVersion,
    frames: Vec<u32>,
    error: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct Index {
    version: u8,
    pub root: PathBuf,
    #[serde(default)]
    pub roots: Vec<IndexedRoot>,
    items: Vec<Item>,
    warnings: Vec<String>,
}

impl Index {
    pub fn file_path(&self, file: &FileId) -> Option<PathBuf> {
        let root = if file.folder_id.is_empty() {
            self.roots
                .first()
                .map(|entry| entry.path.as_path())
                .unwrap_or(self.root.as_path())
        } else {
            self.roots
                .iter()
                .find(|entry| entry.folder_id == file.folder_id)
                .map(|entry| entry.path.as_path())?
        };
        Some(file.path(root))
    }

    pub fn available(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.error.is_none())
            .count()
    }

    pub fn failures(&self) -> usize {
        self.items.len() - self.available() + self.warnings.len()
    }

    pub fn failures_preview(&self) -> Vec<String> {
        self.warnings
            .iter()
            .cloned()
            .chain(self.items.iter().filter_map(|item| {
                item.error
                    .as_ref()
                    .map(|error| format!("{}：{error}", item.file.name))
            }))
            .take(10)
            .collect()
    }

    #[cfg(test)]
    pub fn load(path: &Path, root: &Path) -> Option<Self> {
        let file = File::open(path).ok()?;
        let index: Self = serde_json::from_reader(BufReader::new(file)).ok()?;
        (index.version == VERSION && index.root == root).then_some(index)
    }

    pub fn load_many(path: &Path, roots: &[IndexedRoot]) -> Option<Self> {
        let file = File::open(path).ok()?;
        let index: Self = serde_json::from_reader(BufReader::new(file)).ok()?;
        (index.version == VERSION && index.roots == roots).then_some(index)
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        bgm_core::persist::write_json(path, self).map_err(|error| error.to_string())
    }

    pub fn duplicates(&self, visible: &HashSet<FileId>) -> Vec<DuplicateGroup> {
        let mut groups = HashMap::<String, Vec<usize>>::new();
        let mut audio = HashMap::<&[u32], Vec<usize>>::new();
        let mut eligible = Vec::new();
        for (position, item) in self.items.iter().enumerate() {
            groups.entry(item.file.key()).or_default().push(position);
            if item.frames.len() >= MIN_HITS {
                audio.entry(&item.frames).or_default().push(position);
                eligible.push(position);
            }
        }
        let mut parent: Vec<usize> = (0..self.items.len()).collect();
        let mut pair_scores = Vec::<(usize, usize, u8)>::new();
        for group in groups.values().chain(audio.values()) {
            if let Some((&first, rest)) = group.split_first() {
                for &other in rest {
                    union(&mut parent, first, other);
                }
            }
        }
        for i in 0..eligible.len() {
            for j in (i + 1)..eligible.len() {
                let left = eligible[i];
                let right = eligible[j];
                if representative(&mut parent, left) == representative(&mut parent, right) {
                    continue;
                }
                let a = &self.items[left].frames;
                let b = &self.items[right].frames;
                let (shorter, longer) = if a.len() <= b.len() { (a, b) } else { (b, a) };
                if let Some((score, _)) = similarity(shorter, longer)
                    && score >= 85
                {
                    union(&mut parent, left, right);
                    pair_scores.push((left, right, score));
                }
            }
        }
        let mut merged = HashMap::<usize, Vec<usize>>::new();
        for position in 0..self.items.len() {
            merged
                .entry(representative(&mut parent, position))
                .or_default()
                .push(position);
        }
        let mut duplicates: Vec<DuplicateGroup> = merged
            .into_values()
            .filter(|positions| positions.len() > 1)
            .map(|positions| {
                let root = representative(&mut parent, positions[0]);
                let sim = pair_scores
                    .iter()
                    .filter(|(left, _, _)| representative(&mut parent, *left) == root)
                    .map(|(_, _, score)| *score)
                    .min()
                    .unwrap_or(100);
                let mut files: Vec<_> = positions
                    .into_iter()
                    .map(|idx| {
                        let item = &self.items[idx];
                        (item.file.clone(), item.version)
                    })
                    .collect();
                files.sort_by(|(a, _), (b, _)| {
                    visible.contains(b).cmp(&visible.contains(a)).then(a.cmp(b))
                });
                DuplicateGroup {
                    similarity: sim,
                    files,
                }
            })
            .collect();
        duplicates.sort_by(|a, b| {
            b.similarity
                .cmp(&a.similarity)
                .then(a.files[0].0.cmp(&b.files[0].0))
        });
        duplicates
    }

    pub fn search(
        &self,
        query: Query,
        limit: usize,
        cancelled: &AtomicBool,
    ) -> Result<Vec<Hit>, String> {
        let frames = match query {
            Query::File(path) => file_fingerprint(&path, QUERY_FRAMES, cancelled)?,
            Query::Recording { rate, mono } => {
                let source = SamplesBuffer::new(NonZeroU16::MIN, rate, mono);
                fingerprint(
                    UniformSourceIterator::new(source, NonZeroU16::MIN, sample_rate()),
                    QUERY_FRAMES,
                    cancelled,
                )?
            }
        };
        if frames.iter().filter(|&&frame| frame != 0).count() < MIN_HITS {
            return Err("音频片段太短或缺少可辨认的声音".into());
        }
        let mut hits = Vec::new();
        for item in &self.items {
            if cancelled.load(Ordering::Relaxed) {
                return Err("已取消音频搜索".into());
            }
            if let Some((score, offset_seconds)) = similarity(&frames, &item.frames) {
                hits.push(Hit {
                    file: item.file.clone(),
                    score,
                    offset_seconds,
                });
            }
        }
        hits.sort_by(|a, b| b.score.cmp(&a.score).then(a.file.cmp(&b.file)));
        hits.truncate(limit.clamp(1, 50));
        Ok(hits)
    }
}

fn union(parents: &mut [usize], left: usize, right: usize) {
    let left = representative(parents, left);
    let right = representative(parents, right);
    if left != right {
        parents[right] = left;
    }
}

fn representative(parents: &mut [usize], mut index: usize) -> usize {
    while parents[index] != index {
        parents[index] = parents[parents[index]];
        index = parents[index];
    }
    index
}

#[derive(Clone, Debug)]
pub struct DuplicateGroup {
    pub similarity: u8,
    pub files: Vec<(FileId, FileVersion)>,
}

pub struct Hit {
    pub file: FileId,
    pub score: u8,
    pub offset_seconds: f64,
}

pub enum Query {
    File(PathBuf),
    Recording { rate: NonZeroU32, mono: Vec<f32> },
}

#[derive(Default)]
pub struct Progress {
    pub complete: AtomicUsize,
    pub total: AtomicUsize,
    pub cancelled: AtomicBool,
}

#[cfg(test)]
pub fn refresh(
    root: PathBuf,
    categories: &[String],
    previous: Option<&Index>,
    retry: bool,
    progress: &Progress,
) -> Result<Option<Index>, String> {
    let single = vec![IndexedRoot {
        folder_id: String::new(),
        path: root,
        legacy_key: true,
    }];
    refresh_many(&single, categories, previous, retry, progress)
}

pub fn refresh_many(
    roots: &[IndexedRoot],
    categories: &[String],
    previous: Option<&Index>,
    retry: bool,
    progress: &Progress,
) -> Result<Option<Index>, String> {
    if roots.is_empty() {
        return Ok(None);
    }
    let canonical_roots: Vec<PathBuf> = roots
        .iter()
        .filter_map(|root| fs::canonicalize(&root.path).ok())
        .collect();
    let canonical_set: HashSet<PathBuf> = canonical_roots.iter().cloned().collect();
    let category_set: HashSet<&str> = categories.iter().map(String::as_str).collect();
    let mut seen_files = HashSet::new();
    let mut files: Vec<(FileId, FileVersion, PathBuf)> = Vec::new();
    let mut warnings = Vec::new();

    for root in roots {
        let canonical = match fs::canonicalize(&root.path) {
            Ok(canonical) => canonical,
            Err(error) => {
                warnings.push(format!("{}：{error}", root.path.display()));
                continue;
            }
        };
        for category in categories {
            let dir = root.path.join(category);
            if fs::symlink_metadata(&dir).is_ok_and(|meta| meta.is_symlink()) {
                warnings.push(format!("{category}：音频索引跳过符号链接分类目录"));
                continue;
            }
            let real = match fs::canonicalize(&dir) {
                Ok(real) if real.parent() == Some(canonical.as_path()) => real,
                Ok(_) => {
                    warnings.push(format!("{category}：分类目录位于曲库之外"));
                    continue;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    warnings.push(format!("{category}：{error}"));
                    continue;
                }
            };
            if real != canonical && canonical_set.contains(&real) {
                continue;
            }
            if let Ok(entries) = fs::read_dir(&real) {
                for entry in entries {
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(error) => {
                            warnings.push(format!("{category}：{error}"));
                            continue;
                        }
                    };
                    let Ok(name) = entry.file_name().into_string() else {
                        continue;
                    };
                    if !bgm_core::library::has_audio_extension(&name) {
                        continue;
                    }
                    let path = entry.path();
                    let Ok(real_file) = fs::canonicalize(&path) else {
                        continue;
                    };
                    if !seen_files.insert(real_file) {
                        continue;
                    }
                    if let Some(version) = FileVersion::read(&path) {
                        let stem = Path::new(&name)
                            .file_stem()
                            .and_then(|stem| stem.to_str())
                            .unwrap_or(&name);
                        let song_key = if root.legacy_key {
                            format!("{category}/{stem}")
                        } else {
                            crate::library::folder_category_key(&root.folder_id, category, stem)
                        };
                        files.push((
                            FileId {
                                folder_id: root.folder_id.clone(),
                                category: category.clone(),
                                name,
                                song_key,
                            },
                            version,
                            path,
                        ));
                    }
                }
            }
        }

        if !root.legacy_key {
            collect_uncategorized_index_files(
                &root.path,
                &root.path,
                &canonical,
                &root.folder_id,
                &category_set,
                &canonical_set,
                &mut seen_files,
                &mut files,
                &mut warnings,
            );
        }
    }

    files.sort_by(|(a, _, _), (b, _, _)| a.cmp(b));
    let old = previous
        .filter(|index| index.roots == roots)
        .map_or(&[][..], |index| index.items.as_slice());
    if previous.is_some_and(|index| index.roots == roots && index.warnings == warnings)
        && old.len() == files.len()
        && old.iter().zip(&files).all(|(item, (file, version, _))| {
            item.file == *file && item.version == *version && (!retry || item.error.is_none())
        })
    {
        return Ok(None);
    }

    let cached: HashMap<_, _> = old.iter().map(|item| (&item.file, item)).collect();
    progress.total.store(files.len(), Ordering::Relaxed);
    let mut items = Vec::with_capacity(files.len());
    for (file, version, path) in files {
        if progress.cancelled.load(Ordering::Relaxed) {
            return Err("已取消索引".into());
        }
        let item = match cached.get(&file) {
            Some(item) if item.version == version && (!retry || item.error.is_none()) => {
                (*item).clone()
            }
            _ => {
                let result =
                    file_fingerprint(&path, MAX_FRAMES, &progress.cancelled).and_then(|frames| {
                        if FileVersion::read(&path) != Some(version) {
                            return Err("文件在扫描时发生变化".into());
                        }
                        Ok(frames)
                    });
                match result {
                    Ok(frames) => Item {
                        file,
                        version,
                        frames,
                        error: None,
                    },
                    Err(error) => Item {
                        file,
                        version,
                        frames: Vec::new(),
                        error: Some(error),
                    },
                }
            }
        };
        items.push(item);
        progress.complete.store(items.len(), Ordering::Relaxed);
    }
    if progress.cancelled.load(Ordering::Relaxed) {
        return Err("已取消索引".into());
    }
    Ok(Some(Index {
        version: VERSION,
        root: roots
            .first()
            .map(|entry| entry.path.clone())
            .unwrap_or_default(),
        roots: roots.to_vec(),
        items,
        warnings,
    }))
}

#[allow(clippy::too_many_arguments)]
fn collect_uncategorized_index_files(
    base: &Path,
    dir: &Path,
    base_canonical: &Path,
    folder_id: &str,
    categories: &HashSet<&str>,
    other_roots: &HashSet<PathBuf>,
    seen_files: &mut HashSet<PathBuf>,
    out: &mut Vec<(FileId, FileVersion, PathBuf)>,
    warnings: &mut Vec<String>,
) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(format!("{}：{error}", dir.display()));
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            if dir == base && categories.contains(name.as_str()) {
                continue;
            }
            if let Ok(real) = fs::canonicalize(&path)
                && real != base_canonical
                && !other_roots.contains(&real)
            {
                collect_uncategorized_index_files(
                    base,
                    &path,
                    base_canonical,
                    folder_id,
                    categories,
                    other_roots,
                    seen_files,
                    out,
                    warnings,
                );
            }
        } else if meta.is_file() && bgm_core::library::has_audio_extension(&name) {
            let Ok(real_file) = fs::canonicalize(&path) else {
                continue;
            };
            if !seen_files.insert(real_file) {
                continue;
            }
            if let Some(version) = FileVersion::read(&path)
                && let Ok(rel) = path.strip_prefix(base)
                && let Some(rel_str) = rel.to_str()
            {
                let rel_no_ext = rel.with_extension("");
                let rel_stem = rel_no_ext.to_string_lossy().replace('\\', "/");
                let song_key = crate::library::folder_uncategorized_key(folder_id, &rel_stem);
                out.push((
                    FileId {
                        folder_id: folder_id.to_owned(),
                        category: crate::library::UNCATEGORIZED_CATEGORY_ID.to_owned(),
                        name: rel_str.replace('\\', "/"),
                        song_key,
                    },
                    version,
                    path,
                ));
            }
        }
    }
}

fn sample_rate() -> NonZeroU32 {
    NonZeroU32::new(RATE).unwrap()
}

fn file_fingerprint(path: &Path, limit: usize, cancelled: &AtomicBool) -> Result<Vec<u32>, String> {
    let file = File::open(path).map_err(|error| format!("无法打开音频：{error}"))?;
    let source = Decoder::try_from(file).map_err(|error| format!("无法解码音频：{error}"))?;
    let samples = UniformSourceIterator::new(source, NonZeroU16::MIN, sample_rate());
    fingerprint(samples, limit, cancelled)
}

fn fingerprint(
    mut samples: impl Iterator<Item = f32>,
    limit: usize,
    cancelled: &AtomicBool,
) -> Result<Vec<u32>, String> {
    let taper: [f32; WINDOW] = std::array::from_fn(|i| {
        0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / WINDOW as f32).cos()
    });
    let twiddles: [(f32, f32); WINDOW / 2] = std::array::from_fn(|i| {
        let (sin, cos) = (-std::f32::consts::TAU * i as f32 / WINDOW as f32).sin_cos();
        (cos, sin)
    });
    let edges: [usize; BANDS + 1] = std::array::from_fn(|i| {
        (14.0_f32 * 58.0_f32.powf(i as f32 / BANDS as f32)).round() as usize
    });
    let mut mono = [0.0; WINDOW];
    let mut bins = [(0.0, 0.0); WINDOW];
    let mut previous = [0.0; BANDS];
    let mut frames = Vec::new();
    for slot in &mut mono[..HOP] {
        *slot = samples.next().unwrap_or_default();
    }
    while frames.len() < limit {
        if cancelled.load(Ordering::Relaxed) {
            return Err("已取消音频分析".into());
        }
        let mut read = 0;
        for slot in &mut mono[HOP..] {
            let sample = samples.next();
            read += usize::from(sample.is_some());
            *slot = sample.filter(|value| value.is_finite()).unwrap_or_default();
        }
        if read == 0 {
            break;
        }
        for (i, (&sample, &window)) in mono.iter().zip(&taper).enumerate() {
            let target = i.reverse_bits() >> (usize::BITS - WINDOW.ilog2());
            bins[target] = (sample * window, 0.0);
        }
        for power in 1..=WINDOW.ilog2() {
            let span = 1 << power;
            for chunk in bins.chunks_exact_mut(span) {
                let (left, right) = chunk.split_at_mut(span / 2);
                for (i, (a, b)) in left.iter_mut().zip(right).enumerate() {
                    let (cos, sin) = twiddles[i * WINDOW / span];
                    let re = b.0 * cos - b.1 * sin;
                    let im = b.0 * sin + b.1 * cos;
                    *b = (a.0 - re, a.1 - im);
                    *a = (a.0 + re, a.1 + im);
                }
            }
        }
        let energy: [f32; BANDS] = std::array::from_fn(|i| {
            bins[edges[i]..edges[i + 1]]
                .iter()
                .map(|(re, im)| re * re + im * im)
                .sum::<f32>()
        });
        let mut code = 0;
        if energy.iter().sum::<f32>() > 0.003 {
            let bands = energy.map(|value| (value + 1e-6).ln());
            for i in 0..BANDS - 1 {
                let difference = bands[i] - bands[i + 1];
                let old = previous[i] - previous[i + 1];
                code |= u32::from(difference > 0.0) << i;
                code |= u32::from(difference > old) << (i + 16);
            }
            previous = bands;
        }
        frames.push(code);
        mono.copy_within(HOP.., 0);
    }
    if frames.iter().all(|&value| value == 0) {
        return Err("未找到可分析的音频".into());
    }
    Ok(frames)
}

fn similarity(query: &[u32], candidate: &[u32]) -> Option<(u8, f64)> {
    let active: Vec<_> = query
        .iter()
        .enumerate()
        .filter(|(_, code)| **code != 0)
        .collect();
    let stride = (active.len() / 36).max(1);
    let mut best = 0u32;
    let mut best_offset = 0isize;
    for offset in
        -(query.len() as isize - MIN_HITS as isize)..=candidate.len() as isize - MIN_HITS as isize
    {
        let mut bits = 0;
        let mut compared = 0;
        for &(i, &code) in active.iter().step_by(stride) {
            let pos = (i as isize + offset) as usize;
            if let Some(&other) = candidate.get(pos).filter(|&&other| other != 0) {
                bits += (code ^ other).count_ones();
                compared += 1;
            }
        }
        if compared * 4 >= active.len().div_ceil(stride) * 3
            && bits * 100 <= 29 * compared as u32 * 32
        {
            let (bits, count) = active
                .iter()
                .filter_map(|&(i, &code)| {
                    candidate
                        .get((i as isize + offset) as usize)
                        .filter(|&&other| other != 0)
                        .map(|&other| (code ^ other).count_ones())
                })
                .fold((0, 0), |(bits, count), distance| {
                    (bits + distance, count + 1)
                });
            if count >= MIN_HITS as u32 && count as usize * 4 >= active.len() * 3 {
                let score = 100 - bits * 100 / (32 * count);
                if score > best {
                    best = score;
                    best_offset = offset;
                }
            }
        }
    }
    (best >= 74).then(|| {
        let seconds = (best_offset.max(0) as f64) * (HOP as f64 / RATE as f64);
        (best as u8, seconds)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic(seconds: usize, seed: u64) -> Vec<f32> {
        let mut rng = fastrand::Rng::with_seed(seed);
        let phrase = (0..seconds * 10)
            .map(|_| std::array::from_fn::<f32, 4, _>(|_| rng.u32(180..3500) as f32))
            .collect::<Vec<_>>();
        (0..seconds * RATE as usize)
            .map(|i| {
                let time = i as f32 / RATE as f32;
                let notes = phrase[(i * 10 / RATE as usize).min(phrase.len() - 1)];
                notes
                    .into_iter()
                    .enumerate()
                    .map(|(i, frequency)| {
                        (std::f32::consts::TAU * frequency * time).sin() * (0.2 / (i + 1) as f32)
                    })
                    .sum()
            })
            .collect()
    }

    #[test]
    fn audio_index_matches_offset_and_gain_and_rejects_unrelated_audio() {
        let samples = synthetic(13, 10);
        let different = synthetic(13, 20);
        let cancelled = AtomicBool::new(false);
        let analyze =
            |samples: Vec<f32>| fingerprint(samples.into_iter(), MAX_FRAMES, &cancelled).unwrap();
        let start = RATE as usize * 4 + 310;
        let query = samples[start..start + RATE as usize * 5]
            .iter()
            .map(|sample| *sample * 0.31)
            .collect::<Vec<_>>();
        let query = analyze(query);
        let matched = similarity(&query, &analyze(samples)).unwrap();
        assert!(matched.0 >= 82);
        assert!((matched.1 - 4.0).abs() < 0.6, "offset={}", matched.1);
        assert!(similarity(&query, &analyze(different)).is_none());
    }

    #[test]
    fn indexes_hidden_copies_and_does_not_load_a_cache_for_another_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("library");
        let dir = root.join("epic");
        let other_dir = root.join("gufeng");
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(&other_dir).unwrap();
        let source = synthetic(6, 12);
        let scaled: Vec<f32> = source.iter().map(|s| s * 0.6).collect();
        crate::library::tts::write_wave(&dir.join("same.wav"), RATE, &source, false).unwrap();
        fs::copy(dir.join("same.wav"), dir.join("same.mp3")).unwrap();
        crate::library::tts::write_wave(&other_dir.join("renamed_copy.wav"), RATE, &scaled, false)
            .unwrap();
        let groups = [String::from("epic"), String::from("gufeng")];
        let progress = Progress::default();
        let index = refresh(root.clone(), &groups, None, false, &progress)
            .unwrap()
            .unwrap();
        let selected = FileId {
            folder_id: String::new(),
            category: "epic".into(),
            name: "same.wav".into(),
            song_key: "epic/same".into(),
        };
        let duplicates = index.duplicates(&HashSet::from([selected.clone()]));
        assert_eq!(duplicates.len(), 1);
        assert_eq!(duplicates[0].files[0].0, selected);
        assert_eq!(duplicates[0].files.len(), 3);
        assert!(duplicates[0].similarity >= 85);
        let cache = temp.path().join("index.json");
        index.save(&cache).unwrap();
        assert!(Index::load(&cache, &root).is_some());
        assert!(Index::load(&cache, &temp.path().join("other")).is_none());
        assert!(
            refresh(root, &groups, Some(&index), false, &Progress::default())
                .unwrap()
                .is_none()
        );
    }
}
