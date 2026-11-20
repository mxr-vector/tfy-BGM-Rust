//! 轻量 ID3 读取：只解析智能随机与时长展示需要的字段。
//!
//! 曲库扫描会在每次 watch 事件后重新遍历目录，因此这里按「路径 + 大小 + 修改时间」
//! 缓存解析结果；文件被替换时缓存自然失效。解析失败一律返回空标签，不影响曲目本身。

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

/// 单个文件的标签缓存上限，避免异常曲库无限增长。
const MAX_CACHE_ENTRIES: usize = 20_000;
/// 读取文件头部的上限；大部分封面图标签都在 1 MiB 以内。
const MAX_HEAD_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackTags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    /// 来自 TLEN 或 MPEG 帧估算的时长（毫秒）。
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

/// 读取曲目标签；任何异常都退化为空标签。
pub fn read_tags(path: &Path) -> TrackTags {
    let mut tags = TrackTags::default();
    let mut head = Vec::new();
    if let Ok(file) = File::open(path) {
        let _ = file.take(MAX_HEAD_BYTES).read_to_end(&mut head);
    }

    let audio_offset = parse_id3v2(&head, &mut tags);

    if (tags.artist.is_none() || tags.album.is_none() || tags.title.is_none())
        && let Some((title, artist, album)) = read_id3v1(path)
    {
        tags.title.get_or_insert(title);
        tags.artist.get_or_insert(artist);
        tags.album.get_or_insert(album);
    }

    if tags.duration_ms.is_none() {
        let len = fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
        let has_v1 = has_id3v1(path);
        tags.duration_ms = estimate_duration(&head, audio_offset, len, has_v1);
    }

    tags
}

// ---------------------------------------------------------------------------
// ID3v2
// ---------------------------------------------------------------------------

fn parse_id3v2(data: &[u8], tags: &mut TrackTags) -> usize {
    if data.len() < 10 || &data[0..3] != b"ID3" {
        return 0;
    }
    let major = data[3];
    if !(2..=4).contains(&major) {
        return 0;
    }
    let flags = data[5];
    let size = synchsafe(&data[6..10]);
    let footer = if major == 4 && flags & 0x10 != 0 {
        10
    } else {
        0
    };
    let audio_offset = 10 + size + footer;
    let total = audio_offset.min(data.len());

    let mut pos = 10;
    if flags & 0x40 != 0 && major >= 3 && pos + 4 <= total {
        let extended = if major == 4 {
            synchsafe(&data[pos..pos + 4])
        } else {
            be_u32(&data[pos..pos + 4]) as usize
        };
        // v2.3 的扩展头长度不含自身 4 字节，v2.4 才包含。
        let skip = if extended >= 4 {
            if major == 4 { extended } else { extended + 4 }
        } else {
            4
        };
        pos += skip;
    }

    while pos + 6 <= total {
        if major == 2 {
            let id = &data[pos..pos + 3];
            if id.iter().all(|&byte| byte == 0) {
                break;
            }
            let frame_size = ((data[pos + 3] as usize) << 16)
                | ((data[pos + 4] as usize) << 8)
                | data[pos + 5] as usize;
            let body = pos + 6;
            if frame_size == 0 || body + frame_size > total {
                break;
            }
            apply_frame(id, &data[body..body + frame_size], tags);
            pos = body + frame_size;
            continue;
        }

        if pos + 10 > total {
            break;
        }
        let id = &data[pos..pos + 4];
        if id[0] == 0 {
            break;
        }
        let frame_size = if major == 4 {
            synchsafe(&data[pos + 4..pos + 8])
        } else {
            be_u32(&data[pos + 4..pos + 8]) as usize
        };
        let frame_flags = u16::from_be_bytes([data[pos + 8], data[pos + 9]]);
        let body = pos + 10;
        if frame_size == 0 || body + frame_size > total {
            break;
        }

        let unusable = if major == 4 {
            // v2.4: 0x0004 加密、0x0008 压缩、0x0001 带 4 字节长度指示。
            frame_flags & 0x000C != 0
        } else {
            // v2.3: 0x0080 压缩、0x0040 加密。
            frame_flags & 0x00C0 != 0
        };
        if !unusable {
            let mut payload = &data[body..body + frame_size];
            if major == 4 && frame_flags & 0x0001 != 0 && payload.len() >= 4 {
                payload = &payload[4..];
            }
            apply_frame(id, payload, tags);
        }
        pos = body + frame_size;
    }

    audio_offset
}

fn apply_frame(id: &[u8], payload: &[u8], tags: &mut TrackTags) {
    match id {
        b"TIT2" | b"TT2" => set_once(&mut tags.title, decode_text(payload)),
        b"TPE1" | b"TP1" => set_once(&mut tags.artist, decode_text(payload)),
        b"TALB" | b"TAL" => set_once(&mut tags.album, decode_text(payload)),
        b"TCON" | b"TCO" => set_once(&mut tags.genre, decode_text(payload)),
        b"TLEN" | b"TLE" => {
            if let Some(ms) = decode_text(payload).and_then(|text| text.trim().parse::<u64>().ok())
                && tags.duration_ms.is_none()
            {
                tags.duration_ms = Some(ms.min(u32::MAX as u64) as u32);
            }
        }
        _ => {}
    }
}

fn set_once(slot: &mut Option<String>, value: Option<String>) {
    if slot.is_none() {
        *slot = value;
    }
}

fn decode_text(payload: &[u8]) -> Option<String> {
    let (&encoding, rest) = payload.split_first()?;
    let text = match encoding {
        0 => rest.iter().map(|&byte| byte as char).collect::<String>(),
        1 => decode_utf16(rest),
        2 => decode_utf16_be(rest),
        3 => String::from_utf8_lossy(rest).into_owned(),
        _ => return None,
    };
    let cleaned = text
        .replace('\0', " ")
        .replace('\u{feff}', "")
        .trim()
        .to_owned();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn decode_utf16(bytes: &[u8]) -> String {
    let big_endian = bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF;
    let little_endian = bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE;
    let body = if big_endian || little_endian {
        &bytes[2..]
    } else {
        bytes
    };
    let units: Vec<u16> = body
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            if big_endian {
                u16::from_be_bytes(*pair)
            } else {
                u16::from_le_bytes(*pair)
            }
        })
        .collect();
    String::from_utf16_lossy(&units)
}

fn decode_utf16_be(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_be_bytes(*pair))
        .collect();
    String::from_utf16_lossy(&units)
}

// ---------------------------------------------------------------------------
// ID3v1
// ---------------------------------------------------------------------------

fn read_id3v1(path: &Path) -> Option<(String, String, String)> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len < 128 {
        return None;
    }
    file.seek(SeekFrom::End(-128)).ok()?;
    let mut buf = [0u8; 128];
    file.read_exact(&mut buf).ok()?;
    if &buf[0..3] != b"TAG" {
        return None;
    }
    let field = |bytes: &[u8]| {
        let end = bytes
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(bytes.len());
        let value = String::from_utf8_lossy(&bytes[..end]).trim().to_owned();
        (!value.is_empty()).then_some(value)
    };
    let title = field(&buf[3..33])?;
    let artist = field(&buf[33..63])?;
    let album = field(&buf[63..93])?;
    Some((title, artist, album))
}

fn has_id3v1(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let Ok(len) = file.metadata().map(|meta| meta.len()) else {
        return false;
    };
    if len < 128 {
        return false;
    }
    if file.seek(SeekFrom::End(-128)).is_err() {
        return false;
    }
    let mut magic = [0u8; 3];
    file.read_exact(&mut magic).is_ok() && &magic == b"TAG"
}

// ---------------------------------------------------------------------------
// 时长估算（Xing 帧数优先，其次按首帧码率做 CBR 估算）
// ---------------------------------------------------------------------------

const BITRATES_V1_L3: [u32; 16] = [
    0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
];
const BITRATES_V2_L3: [u32; 16] = [
    0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0,
];

struct MpegInfo {
    bitrate_bps: u32,
    sample_rate: u32,
    samples_per_frame: u32,
}

fn estimate_duration(head: &[u8], audio_offset: usize, file_len: u64, has_v1: bool) -> Option<u32> {
    let audio_len = file_len
        .saturating_sub(audio_offset as u64)
        .saturating_sub(if has_v1 { 128 } else { 0 });
    if audio_len < 1024 {
        return None;
    }
    let hay = head.get(audio_offset..).unwrap_or(&[]);
    let scan = hay.len().saturating_sub(4).min(64 * 1024);

    for index in 0..scan {
        if hay[index] != 0xFF || hay[index + 1] & 0xE0 != 0xE0 {
            continue;
        }
        let Some(info) = mpeg_info(&hay[index..]) else {
            continue;
        };
        if let Some(frames) = xing_frames(&hay[index..]) {
            let seconds =
                f64::from(frames) * f64::from(info.samples_per_frame) / f64::from(info.sample_rate);
            if (0.5..86_400.0).contains(&seconds) {
                return Some((seconds * 1000.0) as u32);
            }
        }
        if info.bitrate_bps > 0 {
            let seconds = audio_len as f64 * 8.0 / f64::from(info.bitrate_bps);
            if (0.5..86_400.0).contains(&seconds) {
                return Some((seconds * 1000.0) as u32);
            }
        }
    }
    None
}

fn mpeg_info(header: &[u8]) -> Option<MpegInfo> {
    if header.len() < 4 || header[0] != 0xFF || header[1] & 0xE0 != 0xE0 {
        return None;
    }
    let version = (header[1] >> 3) & 0x03;
    let layer = (header[1] >> 1) & 0x03;
    // 曲库只收 mp3，即 Layer III。
    if layer != 0b01 {
        return None;
    }
    let bitrate_index = (header[2] >> 4) as usize;
    let sample_index = ((header[2] >> 2) & 0x03) as usize;
    if bitrate_index == 0 || bitrate_index == 15 || sample_index == 3 {
        return None;
    }

    let (table, base_rate, samples_per_frame) = match version {
        0b11 => (
            &BITRATES_V1_L3,
            [44_100, 48_000, 32_000][sample_index],
            1152,
        ),
        0b10 => (&BITRATES_V2_L3, [22_050, 24_000, 16_000][sample_index], 576),
        0b00 => (&BITRATES_V2_L3, [11_025, 12_000, 8_000][sample_index], 576),
        _ => return None,
    };

    Some(MpegInfo {
        bitrate_bps: table[bitrate_index] * 1000,
        sample_rate: base_rate,
        samples_per_frame,
    })
}

fn xing_frames(frame: &[u8]) -> Option<u32> {
    let limit = frame.len().min(256);
    let index = frame[..limit]
        .windows(4)
        .position(|window| window == b"Xing" || window == b"Info")?;
    let flags_at = index + 4;
    if flags_at + 8 > frame.len() {
        return None;
    }
    let flags = be_u32(&frame[flags_at..flags_at + 4]);
    if flags & 0x01 == 0 {
        return None;
    }
    Some(be_u32(&frame[flags_at + 4..flags_at + 8]))
}

// ---------------------------------------------------------------------------
// 字节工具
// ---------------------------------------------------------------------------

fn synchsafe(bytes: &[u8]) -> usize {
    bytes.iter().fold(0usize, |value, &byte| {
        (value << 7) | usize::from(byte & 0x7F)
    })
}

fn be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn frame_v3(id: &[u8; 4], text: &str) -> Vec<u8> {
        let mut payload = vec![3u8];
        payload.extend_from_slice(text.as_bytes());
        let mut frame = Vec::new();
        frame.extend_from_slice(id);
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(&[0, 0]);
        frame.extend_from_slice(&payload);
        frame
    }

    fn tag_with_frames(frames: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = frames.iter().flatten().copied().collect();
        let size = body.len();
        let synchsafe = [
            ((size >> 21) & 0x7F) as u8,
            ((size >> 14) & 0x7F) as u8,
            ((size >> 7) & 0x7F) as u8,
            (size & 0x7F) as u8,
        ];
        let mut tag = b"ID3\x03\x00\x00".to_vec();
        tag.extend_from_slice(&synchsafe);
        tag.extend_from_slice(&body);
        tag
    }

    #[test]
    fn parses_text_frames() {
        let tag = tag_with_frames(&[
            frame_v3(b"TIT2", "Buttercup"),
            frame_v3(b"TPE1", "Jack Stauber"),
            frame_v3(b"TALB", "Pop Food"),
            frame_v3(b"TCON", "Indie"),
        ]);
        let mut tags = TrackTags::default();
        parse_id3v2(&tag, &mut tags);

        assert_eq!(tags.title.as_deref(), Some("Buttercup"));
        assert_eq!(tags.artist.as_deref(), Some("Jack Stauber"));
        assert_eq!(tags.album.as_deref(), Some("Pop Food"));
        assert_eq!(tags.genre.as_deref(), Some("Indie"));
    }

    #[test]
    fn parses_tlen_duration() {
        let tag = tag_with_frames(&[frame_v3(b"TLEN", "183500")]);
        let mut tags = TrackTags::default();
        parse_id3v2(&tag, &mut tags);
        assert_eq!(tags.duration_ms, Some(183_500));
    }

    #[test]
    fn skips_v23_extended_header_before_frames() {
        // v2.3 扩展头：size 字段(6，不含自身) + flags(2) + padding size(4) = 10 字节。
        let mut ext = Vec::new();
        ext.extend_from_slice(&6u32.to_be_bytes());
        ext.extend_from_slice(&[0u8, 0u8]);
        ext.extend_from_slice(&0u32.to_be_bytes());
        let body: Vec<u8> = ext
            .into_iter()
            .chain(frame_v3(b"TIT2", "WithExt"))
            .collect();
        let size = body.len();
        let synchsafe = [
            ((size >> 21) & 0x7F) as u8,
            ((size >> 14) & 0x7F) as u8,
            ((size >> 7) & 0x7F) as u8,
            (size & 0x7F) as u8,
        ];
        let mut tag = b"ID3\x03\x00\x40".to_vec();
        tag.extend_from_slice(&synchsafe);
        tag.extend_from_slice(&body);

        let mut tags = TrackTags::default();
        parse_id3v2(&tag, &mut tags);
        assert_eq!(tags.title.as_deref(), Some("WithExt"));
    }

    #[test]
    fn decodes_utf16_with_bom() {
        let mut payload = vec![1u8, 0xFF, 0xFE];
        for unit in "听风吟".encode_utf16() {
            payload.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(decode_text(&payload).as_deref(), Some("听风吟"));
    }

    #[test]
    fn reads_tags_from_file_and_skips_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let tagged = dir.path().join("tagged.mp3");
        let mut bytes =
            tag_with_frames(&[frame_v3(b"TPE1", "Artist A"), frame_v3(b"TALB", "Album A")]);
        bytes.extend_from_slice(b"\xFF\xFB\x90\x00");
        fs::write(&tagged, bytes).unwrap();

        let tags = read_tags(&tagged);
        assert_eq!(tags.artist.as_deref(), Some("Artist A"));
        assert_eq!(tags.album.as_deref(), Some("Album A"));

        let plain = dir.path().join("plain.mp3");
        fs::write(&plain, b"not audio at all").unwrap();
        let tags = read_tags(&plain);
        assert_eq!(tags, TrackTags::default());
    }

    #[test]
    fn meta_cache_reuses_entry_until_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cached.mp3");
        fs::write(&path, tag_with_frames(&[frame_v3(b"TPE1", "One")])).unwrap();
        let cache = MetaCache::default();
        let meta = fs::metadata(&path).unwrap();

        assert_eq!(cache.get(&path, &meta).artist.as_deref(), Some("One"));

        fs::write(&path, tag_with_frames(&[frame_v3(b"TPE1", "Two")])).unwrap();
        let meta = fs::metadata(&path).unwrap();
        assert_eq!(cache.get(&path, &meta).artist.as_deref(), Some("Two"));
    }
}
