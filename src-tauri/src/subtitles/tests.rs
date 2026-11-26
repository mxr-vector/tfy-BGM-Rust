//! 字幕格式化、解析与迁移的单元测试。

use std::path::PathBuf;

use bgm_core::Song;

use super::format::MAX_SEGMENT_TEXT_CHARS;
use super::format::*;
use super::store::*;
use super::*;

fn sample() -> Vec<SubtitleSegment> {
    vec![
        SubtitleSegment {
            start: 0.0,
            end: 10.0,
            text: "第一句歌词".to_owned(),
        },
        SubtitleSegment {
            start: 10.0,
            end: 20.5,
            text: "第二句歌词".to_owned(),
        },
    ]
}

#[test]
fn srt_has_index_timecodes_and_text() {
    let srt = format_srt(&sample());
    assert_eq!(
        srt,
        concat!(
            "1\n00:00:00,000 --> 00:00:10,000\n第一句歌词\n\n",
            "2\n00:00:10,000 --> 00:00:20,500\n第二句歌词\n\n",
        )
    );
}

#[test]
fn lrc_has_title_and_centisecond_timecodes() {
    let lrc = format_lrc("测试曲目", &sample());
    assert!(lrc.starts_with("[ar:]\n[ti:测试曲目]\n"));
    assert!(lrc.contains("[00:00.00]第一句歌词\n"));
    assert!(lrc.contains("[00:10.00]第二句歌词\n"));
}

#[test]
fn time_helpers_round_and_clamp() {
    assert_eq!(srt_time(0.0), "00:00:00,000");
    assert_eq!(srt_time(3661.234), "01:01:01,234");
    assert_eq!(srt_time(-5.0), "00:00:00,000");
    assert_eq!(lrc_time(61.25), "01:01.25");
}

#[test]
fn migration_rewrites_key_title_and_texts() {
    let stored = StoredSubtitle {
        key: "ambient/old_name".to_owned(),
        title: "旧标题".to_owned(),
        segments: sample(),
        created_at_ms: 42,
        updated_at_ms: None,
        file_name: Some("old_name.mp3".to_owned()),
        duration_ms: Some(1_000),
        srt: String::new(),
        lrc: String::new(),
        separation_model_name: None,
        accompaniment_model_name: None,
        vocals_path: None,
        accompaniment_path: None,
        accompaniment_kind: None,
        srt_path: None,
        lrc_path: None,
    };
    let song = Song {
        key: "epic/new_name".to_owned(),
        title: "新标题".to_owned(),
        file_name: "new_name.mp3".to_owned(),
        category: "epic".to_owned(),
        path: "D:/music/epic/new_name.mp3".to_owned(),
        artist: None,
        album: None,
        genre: None,
        duration_ms: Some(62_000),
    };
    let next = migrated_stored(&stored, &song.key, Some(&song));
    assert_eq!(next.key, "epic/new_name");
    assert_eq!(next.title, "新标题");
    assert_eq!(next.file_name.as_deref(), Some("new_name.mp3"));
    assert_eq!(next.duration_ms, Some(62_000));
    // 分段与生成时间保持不变，预览文本按新标题重建。
    assert_eq!(next.segments, stored.segments);
    assert_eq!(next.created_at_ms, 42);
    assert!(next.lrc.starts_with("[ar:]\n[ti:新标题]\n"));
    assert_eq!(next.srt, format_srt(&stored.segments));
}

#[test]
fn migration_remaps_separation_paths_only_inside_old_dir() {
    let from = "ambient/old_name";
    let old_dir = PathBuf::from("C:/config/separations").join(subtitle_stem(from));
    let new_dir = PathBuf::from("C:/config/separations").join(subtitle_stem("epic/new_name"));

    let mut vocals = Some(old_dir.join("vocals.wav").to_string_lossy().into_owned());
    remap_separation_path(&mut vocals, from, &new_dir);
    assert_eq!(
        vocals.as_deref(),
        Some(new_dir.join("vocals.wav").to_string_lossy().as_ref())
    );

    // 不在旧分离目录下的自定义路径保持原样。
    let external = Some("D:/deliveries/custom.srt".to_owned());
    let mut untouched = external.clone();
    remap_separation_path(&mut untouched, from, &new_dir);
    assert_eq!(untouched, external);

    let mut empty: Option<String> = None;
    remap_separation_path(&mut empty, from, &new_dir);
    assert!(empty.is_none());
}

#[test]
fn subtitle_stem_is_stable_and_distinct() {
    assert_eq!(subtitle_stem("pop/song.mp3"), subtitle_stem("pop/song.mp3"));
    assert_ne!(
        subtitle_stem("pop/song.mp3"),
        subtitle_stem("pop/song2.mp3")
    );
    assert!(!subtitle_stem("古典/曲 目.flac").contains('/'));
}

#[test]
fn normalize_segments_validates_and_sorts() {
    let normalized = normalize_segments(vec![
        SubtitleSegment {
            start: 10.0,
            end: 12.0,
            text: "  第二句 \n 换行 ".to_owned(),
        },
        SubtitleSegment {
            start: 0.0,
            end: 5.0,
            text: "第一句".to_owned(),
        },
    ])
    .unwrap();
    assert_eq!(normalized[0].start, 0.0);
    assert_eq!(normalized[0].text, "第一句");
    assert!(!normalized[1].text.contains('\n'));
    assert!(normalized[1].text.starts_with("第二句"));

    assert!(normalize_segments(Vec::new()).is_err());
    assert!(
        normalize_segments(vec![SubtitleSegment {
            start: 5.0,
            end: 5.0,
            text: "零长度".to_owned(),
        }])
        .is_err()
    );
    assert!(
        normalize_segments(vec![SubtitleSegment {
            start: -1.0,
            end: 2.0,
            text: "负数".to_owned(),
        }])
        .is_err()
    );
    assert!(
        normalize_segments(vec![SubtitleSegment {
            start: f64::NAN,
            end: 2.0,
            text: "非数字".to_owned(),
        }])
        .is_err()
    );
}

#[test]
fn parse_srt_accepts_missing_indexes_and_multiline_text() {
    let srt = "1\n00:00:01,500 --> 00:00:03.250\n第一句\n第二行\n\n00:04.0 --> 00:06.0\n第三句\n";
    let segments = parse_srt(srt).unwrap();
    assert_eq!(segments.len(), 2);
    assert!((segments[0].start - 1.5).abs() < 1e-9);
    assert!((segments[0].end - 3.25).abs() < 1e-9);
    assert_eq!(segments[0].text, "第一句 第二行");
    assert_eq!(segments[1].start, 4.0);
    assert_eq!(segments[1].text, "第三句");
}

#[test]
fn parse_srt_roundtrips_format_srt() {
    let segments = sample();
    let parsed = parse_srt(&format_srt(&segments)).unwrap();
    assert_eq!(parsed.len(), segments.len());
    assert_eq!(parsed[0].text, "第一句歌词");
    assert_eq!(parsed[1].text, "第二句歌词");
    assert!((parsed[1].end - 20.5).abs() < 1e-9);
}

#[test]
fn parse_srt_rejects_malformed_input() {
    assert!(parse_srt("这不是字幕").is_err());
    assert!(parse_srt("1\n00:00:05,000 --> 00:00:02,000\n结束早于开始\n").is_err());
    assert!(parse_srt("").is_err());
}

#[test]
fn parse_lrc_expands_tags_and_derives_ends() {
    let lrc = "[ar:]\n[ti:测试]\n[00:01.00][00:03.50]同一句\n[00:08.25]末句\n";
    let segments = parse_lrc(lrc, Some(12.0)).unwrap();
    assert_eq!(segments.len(), 3);
    assert_eq!(segments[0].start, 1.0);
    assert_eq!(segments[0].end, 3.5);
    assert_eq!(segments[0].text, "同一句");
    assert_eq!(segments[2].text, "末句");
    assert!((segments[2].end - 12.0).abs() < 1e-9);
}

#[test]
fn parse_lrc_falls_back_when_duration_unknown() {
    let segments = parse_lrc("[00:02.00]唯一一句\n", None).unwrap();
    assert_eq!(segments.len(), 1);
    assert!((segments[0].end - 7.0).abs() < 1e-9);
}

#[test]
fn parse_lrc_rejects_lines_without_timestamp() {
    assert!(parse_lrc("没有时间标签的一行", None).is_err());
    assert!(parse_lrc("[ar:]\n[ti:只有元数据]\n", None).is_err());
}

#[test]
fn parse_lrc_rejects_malformed_tags_instead_of_dropping_lines() {
    // 常见手误：沿用 SRT 的逗号毫秒分隔，不能被当作元数据跳过。
    assert!(parse_lrc("[00:01,500]手误\n", None).is_err());
    // 非白名单标签同样应报错，避免静默丢行。
    assert!(parse_lrc("[Verse 1]歌词\n", None).is_err());
    assert!(parse_lrc("[ab:12]歌词\n", None).is_err());
    // 元数据之后仍有正文却缺时间标签：报错，不能按纯元数据行跳过。
    assert!(parse_lrc("[ar:歌手]没有时间的正文\n", None).is_err());
    // 白名单元数据标签仍被忽略，不影响时间轴。
    let segments = parse_lrc("[ar:歌手][ti:标题]\n[00:03.00]正文\n", None).unwrap();
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].start, 3.0);
    assert_eq!(segments[0].text, "正文");
    // 时间标签之后的方括号属于歌词正文，应原样保留而不是被当作标签吞掉。
    let segments = parse_lrc("[00:05.00][笑]歌词\n", None).unwrap();
    assert_eq!(segments[0].text, "[笑]歌词");
}

#[test]
fn parse_srt_rejects_broken_timeline_after_first_cue() {
    let srt = "1\n00:00:01,000 --> 00:00:02,000\n第一句\n00:00:03,XXX --> 00:00:04,000\n第二句\n";
    let message = parse_srt(srt).unwrap_err().to_string();
    assert!(message.contains("时间轴无法解析"), "{message}");
    assert!(message.contains("第 4 行"), "{message}");
}

#[test]
fn normalize_segments_rejects_overlong_text() {
    let text = "字".repeat(MAX_SEGMENT_TEXT_CHARS + 1);
    let message = normalize_segments(vec![SubtitleSegment {
        start: 0.0,
        end: 1.0,
        text,
    }])
    .unwrap_err()
    .to_string();
    assert!(message.contains("文本过长"), "{message}");
}
