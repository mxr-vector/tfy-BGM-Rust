//! 基于 ffmpeg `loudnorm` 的响度归一化与导出场景预设。
//!
//! 采用 ITU-R BS.1770 / EBU R128 感知响度（按人耳与人声频段加权），
//! 两遍处理：第一遍测量素材真实响度，第二遍按测量值做线性增益，
//! 避免单遍动态模式在音乐素材上产生可闻的“抽吸”感。
//!
//! ffmpeg 的查找顺序：`TFY_FFMPEG` 环境变量 → exe 同级目录 → PATH。

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 未指定或未知场景时使用的默认预设。
pub const DEFAULT_PRESET_ID: &str = "video-editing";

/// 导出场景预设：目标响度按主流平台与广播标准归纳。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportPreset {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    /// 目标整体响度（LUFS）。
    pub target_lufs: f32,
    /// 真峰值上限（dBTP）。
    pub true_peak: f32,
    /// 目标响度范围（LU）。
    pub loudness_range: f32,
}

/// 界面按此顺序展示场景选项，第一项为默认值。
pub const EXPORT_PRESETS: [ExportPreset; 4] = [
    ExportPreset {
        id: DEFAULT_PRESET_ID,
        label: "视频剪辑",
        description: "对齐视频平台的 -14 LUFS 响度，BGM 与人声层次稳定，不易被二次压缩。",
        target_lufs: -14.0,
        true_peak: -1.0,
        loudness_range: 11.0,
    },
    ExportPreset {
        id: "podcast",
        label: "播客/访谈",
        description: "人声清晰、对白优先，适合播客、访谈与口播类节目。",
        target_lufs: -16.0,
        true_peak: -1.5,
        loudness_range: 11.0,
    },
    ExportPreset {
        id: "focus",
        label: "专注/冥想",
        description: "更安静平缓，适合长时间环境背景、学习专注与冥想场景。",
        target_lufs: -20.0,
        true_peak: -2.0,
        loudness_range: 7.0,
    },
    ExportPreset {
        id: "broadcast",
        label: "广播/影视",
        description: "符合 EBU R128 广播标准，适合电视台投放与专业交付。",
        target_lufs: -23.0,
        true_peak: -1.0,
        loudness_range: 7.0,
    },
];

/// 按 id 查找预设；忽略大小写与首尾空白。
pub fn find_preset(id: &str) -> Option<ExportPreset> {
    let normalized = id.trim().to_ascii_lowercase();
    EXPORT_PRESETS
        .iter()
        .copied()
        .find(|preset| preset.id == normalized.as_str())
}

/// 查找预设，未知 id 回退为默认的视频剪辑。
pub fn preset_or_default(id: &str) -> ExportPreset {
    find_preset(id).unwrap_or(EXPORT_PRESETS[0])
}

/// 定位可用的 ffmpeg；每个候选都会执行 `-version` 做存活探测。
pub fn ffmpeg_path() -> Option<PathBuf> {
    ffmpeg_candidates().into_iter().find(|program| can_run(program))
}

/// 用 ffmpeg 两遍 loudnorm 把 `source` 归一化到 `preset`，输出 mp3 到 `destination`。
pub fn normalize_file(
    ffmpeg: &Path,
    source: &Path,
    destination: &Path,
    preset: ExportPreset,
) -> Result<(), LoudnormError> {
    let stats = measure(ffmpeg, source, preset)?;
    let filter = format!(
        "loudnorm=I={}:TP={}:LRA={}:measured_I={}:measured_TP={}:measured_LRA={}:measured_thresh={}:offset={}:linear=true:print_format=summary",
        preset.target_lufs,
        preset.true_peak,
        preset.loudness_range,
        stats.input_i,
        stats.input_tp,
        stats.input_lra,
        stats.input_thresh,
        stats.target_offset,
    );

    let output = build_command(ffmpeg)
        .args(["-hide_banner", "-nostdin", "-y", "-i"])
        .arg(source)
        // 保留内嵌封面；`0:v?` 在无视频流时不会报错。
        .args(["-map", "0:a:0", "-map", "0:v?", "-c:v", "copy"])
        .args(["-af"])
        .arg(&filter)
        .args([
            "-map_metadata",
            "0",
            "-c:a",
            "libmp3lame",
            "-b:a",
            "320k",
            "-id3v2_version",
            "3",
            "-f",
            "mp3",
        ])
        .arg(destination)
        .output()
        .map_err(LoudnormError::Spawn)?;

    if !output.status.success() {
        let _ = std::fs::remove_file(destination);
        return Err(LoudnormError::Encode(tail(&output.stderr)));
    }

    let written = std::fs::metadata(destination)
        .map_err(LoudnormError::Write)?
        .len();
    if written == 0 {
        let _ = std::fs::remove_file(destination);
        return Err(LoudnormError::Write(io::Error::new(
            io::ErrorKind::WriteZero,
            "ffmpeg 输出为空",
        )));
    }
    Ok(())
}

fn measure(
    ffmpeg: &Path,
    source: &Path,
    preset: ExportPreset,
) -> Result<LoudnormStats, LoudnormError> {
    let filter = format!(
        "loudnorm=I={}:TP={}:LRA={}:print_format=json",
        preset.target_lufs, preset.true_peak, preset.loudness_range,
    );
    let output = build_command(ffmpeg)
        .args(["-hide_banner", "-nostdin", "-i"])
        .arg(source)
        .args(["-vn", "-af"])
        .arg(&filter)
        .args(["-f", "null", "-"])
        .output()
        .map_err(LoudnormError::Spawn)?;

    if !output.status.success() {
        return Err(LoudnormError::Analyze(tail(&output.stderr)));
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stats = parse_stats(&stderr).ok_or_else(|| LoudnormError::Parse(tail(&output.stderr)))?;
    if stats.is_finite() {
        Ok(stats)
    } else {
        Err(LoudnormError::Silent)
    }
}

#[derive(Debug, Deserialize)]
struct LoudnormStats {
    input_i: String,
    input_tp: String,
    input_lra: String,
    input_thresh: String,
    target_offset: String,
}

impl LoudnormStats {
    /// 静音素材会得到 `-inf`，此时无法做线性归一化。
    fn is_finite(&self) -> bool {
        [
            &self.input_i,
            &self.input_tp,
            &self.input_lra,
            &self.input_thresh,
            &self.target_offset,
        ]
        .iter()
        .all(|value| {
            value
                .trim()
                .parse::<f64>()
                .is_ok_and(|number| number.is_finite())
        })
    }
}

/// 从 ffmpeg stderr 的末尾 JSON 块中提取 loudnorm 测量值。
fn parse_stats(stderr: &str) -> Option<LoudnormStats> {
    let key = stderr.rfind("\"input_i\"")?;
    let start = stderr[..key].rfind('{')?;
    let end = stderr[key..].find('}')? + key;
    serde_json::from_str(&stderr[start..=end]).ok()
}

fn ffmpeg_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(raw) = std::env::var_os("TFY_FFMPEG") {
        let path = PathBuf::from(raw);
        candidates.push(if path.is_dir() {
            path.join(ffmpeg_program())
        } else {
            path
        });
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        candidates.push(dir.join(ffmpeg_program()));
    }
    candidates.push(PathBuf::from(ffmpeg_program()));
    candidates
}

fn ffmpeg_program() -> &'static str {
    if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" }
}

fn can_run(program: &Path) -> bool {
    build_command(program)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn build_command(program: &Path) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // 绿色版不弹控制台窗口。
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

fn tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim();
    let start = text
        .char_indices()
        .rev()
        .nth(600)
        .map_or(0, |(index, _)| index);
    text[start..].replace('\n', " ")
}

#[derive(Debug, Error)]
pub enum LoudnormError {
    #[error("无法启动 ffmpeg：{0}")]
    Spawn(#[source] io::Error),
    #[error("ffmpeg 响度检测失败：{0}")]
    Analyze(String),
    #[error("无法解析 ffmpeg 响度数据：{0}")]
    Parse(String),
    #[error("音频长时间接近静音，已跳过响度归一化")]
    Silent,
    #[error("ffmpeg 响度归一化失败：{0}")]
    Encode(String),
    #[error("写入导出文件失败：{0}")]
    Write(#[source] io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn presets_have_unique_ids_and_default_first() {
        let mut ids: Vec<&str> = EXPORT_PRESETS.iter().map(|preset| preset.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), EXPORT_PRESETS.len());
        assert_eq!(EXPORT_PRESETS[0].id, DEFAULT_PRESET_ID);
    }

    #[test]
    fn unknown_scene_falls_back_to_default() {
        assert_eq!(preset_or_default("podcast").id, "podcast");
        assert_eq!(preset_or_default(" PODCAST ").id, "podcast");
        assert_eq!(preset_or_default("unknown").id, DEFAULT_PRESET_ID);
        assert_eq!(preset_or_default("").target_lufs, -14.0);
    }

    #[test]
    fn parses_loudnorm_json_from_stderr() {
        let stderr = r#"ffmpeg version 8
[Parsed_loudnorm_0 @ 0000] 
{
	"input_i" : "-23.01",
	"input_tp" : "-2.65",
	"input_lra" : "7.30",
	"input_thresh" : "-33.53",
	"output_i" : "-14.00",
	"target_offset" : "0.12"
}
"#;
        let stats = parse_stats(stderr).unwrap();
        assert_eq!(stats.input_i, "-23.01");
        assert_eq!(stats.target_offset, "0.12");
        assert!(stats.is_finite());
    }

    #[test]
    fn silence_is_not_finite() {
        let stats = LoudnormStats {
            input_i: "-inf".into(),
            input_tp: "-inf".into(),
            input_lra: "0.0".into(),
            input_thresh: "-inf".into(),
            target_offset: "0.0".into(),
        };
        assert!(!stats.is_finite());
    }

    #[test]
    fn preset_actually_changes_output_loudness_when_ffmpeg_is_available() {
        let Some(ffmpeg) = ffmpeg_path() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("tone.mp3");

        let generated = build_command(&ffmpeg)
            .args(["-hide_banner", "-nostdin", "-y", "-f", "lavfi", "-i"])
            .arg("sine=frequency=440:duration=3")
            .args(["-af", "volume=-10dB", "-c:a", "libmp3lame", "-b:a", "192k"])
            .arg(&input)
            .output()
            .unwrap();
        assert!(generated.status.success(), "{}", tail(&generated.stderr));
        assert!(fs::metadata(&input).unwrap().len() > 0);

        let mut measured_lufs = Vec::new();
        for id in [DEFAULT_PRESET_ID, "focus"] {
            let preset = preset_or_default(id);
            let output = dir.path().join(format!("{id}.mp3"));
            normalize_file(&ffmpeg, &input, &output, preset).unwrap();
            assert!(fs::metadata(&output).unwrap().len() > 0);

            let measured = measure(&ffmpeg, &output, preset).unwrap();
            let lufs: f64 = measured.input_i.parse().unwrap();
            assert!(
                (lufs - f64::from(preset.target_lufs)).abs() < 2.0,
                "场景 {id} 归一化后响度偏离目标：{lufs} LUFS"
            );
            measured_lufs.push(lufs);
        }
        assert!(
            (measured_lufs[0] - measured_lufs[1]).abs() >= 4.0,
            "不同场景预设应产生明显不同的响度：{measured_lufs:?}"
        );
    }
}