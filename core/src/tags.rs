//! 音频标签与时长读取：MP3 / M4A / FLAC / WAV / OGG 统一走 `lofty`。
//!
//! 曲库扫描会在每次 watch 事件后重新遍历目录，因此这里按「路径 + 大小 + 修改时间」
//! 缓存解析结果；文件被替换时缓存自然失效。解析失败一律返回空标签，不影响曲目本身。
//!
//! 不同容器的标签格式由 lofty 内部处理：MP3 走 ID3v2 / ID3v1，M4A 走 iTunes `ilst`，
//! FLAC / OGG 走 Vorbis Comment，WAV 走 RIFF INFO；时长同样由各格式头部字段计算，
//! 无需再为单一容器维护专用解析器。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use lofty::config::ParseOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::probe::Probe;
use lofty::tag::Accessor;

/// 单个文件的标签缓存上限，避免异常曲库无限增长。
const MAX_CACHE_ENTRIES: usize = 20_000;
/// 单个标签值的最大字符数，避免异常文件把超长字符串带进界面。
const MAX_TAG_CHARS: usize = 200;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackTags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    /// 由容器头部（MP3 帧 / FLAC STREAMINFO / WAV fmt / MP4 mvhd / OGG granule）计算。
    pub duration_ms: Option<u32>,
}

/// 带失效校验的标签缓存。
#[derive(Default)]
pub struct MetaCache {
    entries: Mutex<HashMap<PathBuf, CacheEntry>>,
}

struct CacheEntry {
    len: u64,
    modified_ms: u64,
    tags: TrackTags,
}

impl MetaCache {
    pub fn get(&self, path: &Path, meta: &fs::Metadata) -> TrackTags {
        let len = meta.len();
        let modified_ms = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0);

        if let Ok(entries) = self.entries.lock()
            && let Some(cached) = entries.get(path)
            && cached.len == len
            && cached.modified_ms == modified_ms
        {
            return cached.tags.clone();
        }

        let tags = read_tags(path);
        if let Ok(mut entries) = self.entries.lock() {
            if entries.len() >= MAX_CACHE_ENTRIES {
                entries.clear();
            }
            entries.insert(
                path.to_path_buf(),
                CacheEntry {
                    len,
                    modified_ms,
                    tags: tags.clone(),
                },
            );
        }
        tags
    }
}

/// 读取曲目标签与时长；任何异常都退化为空标签。
pub fn read_tags(path: &Path) -> TrackTags {
    // 只需要文本标签与时长，显式关掉封面解析：否则每个新文件都会把内嵌图片
    // 整块读进内存，扫描大曲库时白白放大 IO 与峰值内存。
    let Ok(probe) = Probe::open(path) else {
        return TrackTags::default();
    };
    let Ok(tagged) = probe
        .options(ParseOptions::new().read_cover_art(false))
        .read()
    else {
        return TrackTags::default();
    };

    let mut tags = TrackTags::default();
    let millis = tagged.properties().duration().as_millis();
    if millis > 0 && millis <= u32::MAX as u128 {
        tags.duration_ms = Some(millis as u32);
    }

    if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
        tags.title = clean(tag.title().as_deref());
        tags.artist = clean(tag.artist().as_deref());
        tags.album = clean(tag.album().as_deref());
        tags.genre = clean(tag.genre().as_deref());
    }

    tags
}

/// 去掉首尾空白与空值，并限制长度。
fn clean(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() {
        return None;
    }
    Some(value.chars().take(MAX_TAG_CHARS).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// 生成 1 秒 8-bit 单声道静音 WAV；文件足够小，不需要外部素材。
    fn write_wav(path: &Path, seconds: u32) {
        let sample_rate = 8_000_u32;
        let data_len = sample_rate * seconds;
        let mut file = fs::File::create(path).unwrap();
        file.write_all(b"RIFF").unwrap();
        file.write_all(&(36 + data_len).to_le_bytes()).unwrap();
        file.write_all(b"WAVEfmt ").unwrap();
        file.write_all(&16_u32.to_le_bytes()).unwrap();
        file.write_all(&1_u16.to_le_bytes()).unwrap(); // PCM
        file.write_all(&1_u16.to_le_bytes()).unwrap(); // mono
        file.write_all(&sample_rate.to_le_bytes()).unwrap();
        file.write_all(&sample_rate.to_le_bytes()).unwrap(); // byte rate
        file.write_all(&1_u16.to_le_bytes()).unwrap(); // block align
        file.write_all(&8_u16.to_le_bytes()).unwrap(); // bits per sample
        file.write_all(b"data").unwrap();
        file.write_all(&data_len.to_le_bytes()).unwrap();
        file.write_all(&vec![128_u8; data_len as usize]).unwrap();
    }

    #[test]
    fn missing_file_yields_empty_tags() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            read_tags(&dir.path().join("nope.flac")),
            TrackTags::default()
        );
    }

    #[test]
    fn garbage_file_does_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.mp3");
        fs::write(&path, b"not really an mp3").unwrap();
        assert_eq!(read_tags(&path), TrackTags::default());
    }

    #[test]
    fn reads_duration_for_wav() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        write_wav(&path, 2);

        let tags = read_tags(&path);

        let duration = tags.duration_ms.expect("wav duration");
        assert!((1_900..=2_100).contains(&duration), "{duration}");
    }

    #[test]
    fn cache_returns_same_tags() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        write_wav(&path, 1);
        let cache = MetaCache::default();
        let meta = fs::metadata(&path).unwrap();

        let first = cache.get(&path, &meta);
        let second = cache.get(&path, &meta);

        assert_eq!(first, second);
        assert!(first.duration_ms.is_some());
    }
}
