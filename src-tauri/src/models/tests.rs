//! 模型目录、下载源与识别辅助函数的单元测试。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use anyhow::anyhow;
use bgm_core::Prefs;
use bgm_core::paths::MODELS_DIR_NAME;
use sherpa_onnx::Wave;

use super::catalog::*;
use super::download::*;
use super::segment::*;
use super::{
    ASR_SAMPLE_RATE, COMPONENTS, DEFAULT_DOWNLOAD_URL, DEFAULT_MODEL_NAME, LoadPhase,
    ModelLoadStatus, ModelRuntime, QWEN3_FILES, SEGMENT_MIN_SECS, SEGMENT_TARGET_SECS,
};

#[test]
fn default_root_is_under_system_download_dir() {
    assert_eq!(MODELS_DIR_NAME, "bgm-models");
    let root = default_root();
    assert!(root.ends_with(MODELS_DIR_NAME));
    // 默认目录必须挂在系统「下载」目录下，而不是程序安装盘（exe 同级）下。
    let base = dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join("Downloads")))
        .unwrap_or_else(|| PathBuf::from("."));
    assert_eq!(root, base.join(MODELS_DIR_NAME));
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf));
    assert_ne!(root.parent(), exe_dir.as_deref());
}

#[test]
fn custom_root_wins_over_default() {
    let prefs = Prefs {
        models_dir: Some("D:/models".into()),
        ..Prefs::default()
    };
    assert_eq!(root_for(&prefs), PathBuf::from("D:/models"));
}

#[test]
fn scan_groups_onnx_by_directory() {
    let dir = tempfile::tempdir().unwrap();
    let model = dir.path().join(DEFAULT_MODEL_NAME);
    fs::create_dir_all(model.join("tokenizer")).unwrap();
    fs::write(model.join("conv_frontend.onnx"), b"x").unwrap();
    fs::write(model.join("encoder.int8.onnx"), b"xx").unwrap();
    fs::write(model.join("decoder.int8.onnx"), b"xxx").unwrap();
    fs::write(model.join("tokenizer/vocab.json"), b"{}").unwrap();
    fs::write(dir.path().join("single.onnx"), b"x").unwrap();

    let (models, warnings) = scan(dir.path());
    assert!(warnings.is_empty());
    assert_eq!(models.len(), 2);

    let default = models
        .iter()
        .find(|model| model.is_default)
        .expect("default model");
    assert_eq!(default.name, DEFAULT_MODEL_NAME);
    assert_eq!(default.files.len(), 3);
    assert!(default.loadable);

    let single = models
        .iter()
        .find(|model| model.id == "file:single")
        .expect("single file model");
    assert!(!single.loadable);
    // 单文件模型不逐项提示缺失构件，界面按「不支持加载」处理。
    assert!(single.missing.is_empty());
}

#[test]
fn remove_removes_directory_and_attachments() {
    let dir = tempfile::tempdir().unwrap();
    let model = dir.path().join("custom-model");
    fs::create_dir_all(model.join("tokenizer")).unwrap();
    fs::write(model.join("encoder.int8.onnx"), b"x").unwrap();
    fs::write(model.join("tokenizer/vocab.json"), b"{}").unwrap();
    fs::write(dir.path().join("keep.onnx"), b"keep").unwrap();

    ModelRuntime::default()
        .remove(dir.path(), "custom-model")
        .unwrap();
    assert!(!model.exists());
    assert!(dir.path().join("keep.onnx").exists());
}

#[test]
fn remove_removes_only_root_single_file() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("single.onnx"), b"x").unwrap();
    fs::write(dir.path().join("keep.txt"), b"keep").unwrap();

    ModelRuntime::default()
        .remove(dir.path(), "file:single")
        .unwrap();
    assert!(!dir.path().join("single.onnx").exists());
    assert!(dir.path().join("keep.txt").exists());
    assert!(dir.path().exists());
}

#[test]
fn remove_rejects_unknown_or_outside_paths() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("single.onnx"), b"x").unwrap();
    let runtime = ModelRuntime::default();
    assert!(runtime.remove(dir.path(), "../outside").is_err());
    assert!(runtime.remove(dir.path(), "file:missing").is_err());
    assert!(dir.path().join("single.onnx").exists());
}

#[test]
fn remove_rejects_loading_or_downloading_models() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("custom-model")).unwrap();
    fs::write(dir.path().join("custom-model/encoder.int8.onnx"), b"x").unwrap();
    let runtime = ModelRuntime::default();

    runtime.loading.store(true, Ordering::SeqCst);
    assert!(runtime.remove(dir.path(), "custom-model").is_err());
    runtime.loading.store(false, Ordering::SeqCst);
    runtime.downloading.store(true, Ordering::SeqCst);
    assert!(runtime.remove(dir.path(), "custom-model").is_err());
    runtime.downloading.store(false, Ordering::SeqCst);
    assert!(dir.path().join("custom-model").exists());
}

#[test]
fn remove_rejects_active_model_tasks_and_recovers_after_drop() {
    let dir = tempfile::tempdir().unwrap();
    let model = dir.path().join("custom-model");
    fs::create_dir_all(&model).unwrap();
    fs::write(model.join("encoder.int8.onnx"), b"x").unwrap();
    let runtime = ModelRuntime::default();

    let task = runtime.reserve_model_task().unwrap();
    assert!(runtime.remove(dir.path(), "custom-model").is_err());
    assert!(runtime.unload().is_err());
    assert!(runtime.load(dir.path(), &model).is_err());
    assert!(model.exists());

    drop(task);
    runtime.remove(dir.path(), "custom-model").unwrap();
    assert!(!model.exists());
}

#[test]
fn remove_failure_restores_status_when_files_are_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let model = dir.path().join("custom-model");
    fs::create_dir_all(&model).unwrap();
    fs::write(model.join("encoder.int8.onnx"), b"x").unwrap();
    let runtime = ModelRuntime::default();
    let path = model.to_string_lossy().into_owned();
    runtime.set_load_status(ModelLoadStatus {
        phase: LoadPhase::Failed,
        path: Some(path.clone()),
        error: Some("原始加载错误".to_owned()),
    });

    let error = runtime
        .remove_with(dir.path(), "custom-model", |_, _| {
            Err(anyhow!("模拟删除失败"))
        })
        .unwrap_err();

    assert!(error.to_string().contains("模拟删除失败"));
    assert!(model.exists());
    let status = runtime.load_status();
    assert_eq!(status.phase, LoadPhase::Failed);
    assert_eq!(status.path.as_deref(), Some(path.as_str()));
    assert_eq!(status.error.as_deref(), Some("原始加载错误"));
}

#[test]
fn remove_partial_failure_marks_model_as_damaged() {
    let dir = tempfile::tempdir().unwrap();
    let model = dir.path().join("custom-model");
    fs::create_dir_all(&model).unwrap();
    fs::write(model.join("encoder.int8.onnx"), b"x").unwrap();
    let runtime = ModelRuntime::default();
    runtime.set_load_status(ModelLoadStatus {
        phase: LoadPhase::Success,
        path: Some(model.to_string_lossy().into_owned()),
        error: None,
    });

    let error = runtime
        .remove_with(dir.path(), "custom-model", |_, target| {
            fs::remove_file(target.join("encoder.int8.onnx"))?;
            Err(anyhow!("模拟部分删除失败"))
        })
        .unwrap_err();

    assert!(error.to_string().contains("模拟部分删除失败"));
    assert!(!model.join("encoder.int8.onnx").exists());
    let status = runtime.load_status();
    assert_eq!(status.phase, LoadPhase::Failed);
    assert!(status.error.unwrap().contains("模型文件已部分删除"));
}

#[test]
fn default_model_reports_missing_components() {
    let dir = tempfile::tempdir().unwrap();
    let status = default_model(dir.path());
    assert!(!status.installed);
    assert_eq!(status.missing.len(), COMPONENTS.len());

    let model = dir.path().join(DEFAULT_MODEL_NAME);
    fs::create_dir_all(model.join("tokenizer")).unwrap();
    fs::write(model.join("conv_frontend.onnx"), b"x").unwrap();
    let status = default_model(dir.path());
    assert!(!status.installed);
    assert_eq!(status.missing, ["encoder.int8.onnx", "decoder.int8.onnx"]);
}

#[test]
fn parse_modelscope_url_extracts_repo() {
    let source = DownloadSource::parse(DEFAULT_DOWNLOAD_URL).unwrap();
    match source {
        DownloadSource::ModelScope {
            owner,
            repo,
            revision,
        } => {
            assert_eq!(owner, "zengshuishui");
            assert_eq!(repo, "Qwen3-ASR-onnx");
            assert_eq!(revision, "master");
        }
        DownloadSource::Base(_) => panic!("应识别为 ModelScope 仓库"),
    }
}

#[test]
fn file_urls_stay_on_int8_manifest() {
    let source = DownloadSource::parse("https://example.com/mirror/").unwrap();
    let url = source.file_url(&QWEN3_FILES[0]);
    assert_eq!(url, "https://example.com/mirror/conv_frontend.onnx");

    let source = DownloadSource::parse(DEFAULT_DOWNLOAD_URL).unwrap();
    let url = source.file_url(&QWEN3_FILES[2]);
    assert!(url.contains("FilePath=model_0.6B/decoder.int8.onnx"));
    assert!(QWEN3_FILES.iter().all(|file| !file.local.contains("1.7B")));
}

#[test]
fn load_status_starts_idle_and_keeps_failure_reason() {
    let runtime = ModelRuntime::default();
    assert_eq!(runtime.load_status().phase, LoadPhase::Idle);
    runtime.set_load_status(ModelLoadStatus {
        phase: LoadPhase::Failed,
        path: Some("D:/models/demo".to_owned()),
        error: Some("sherpa-onnx 无法加载模型".to_owned()),
    });
    let status = runtime.load_status();
    assert_eq!(status.phase, LoadPhase::Failed);
    assert_eq!(status.error.as_deref(), Some("sherpa-onnx 无法加载模型"));
    // 失败状态下不得被误判为「已加载」，自动加载仍可重试。
    assert!(!runtime.is_loaded_path(Path::new("D:/models/demo")));
}

#[test]
fn clean_asr_text_strips_control_tokens() {
    assert_eq!(
        clean_asr_text("<|im_start|>你好  世界\n第二行<|im_end|>"),
        "你好 世界 第二行"
    );
    assert_eq!(clean_asr_text("  纯文本  "), "纯文本");
    assert_eq!(clean_asr_text("<|im_start|><|endoftext|>"), "");
    assert_eq!(clean_asr_text("<|im_start|><|endoftext|>你好"), "你好");
}

#[test]
fn clean_asr_text_drops_free_generation_after_end_token() {
    // sherpa-onnx 只把 <|im_end|> 当 EOS；Qwen3-ASR 先吐 <|endoftext|> 时
    // 解码会继续自由续写，其后的正文不属于识别结果，必须整段丢弃。
    assert_eq!(
        clean_asr_text(
            "浪漫情歌，春去，小梦不觉。<|endoftext|>Human Rights Watch（人权观察）是全球最大的人权组织。"
        ),
        "浪漫情歌，春去，小梦不觉。"
    );
    assert_eq!(
        clean_asr_text("离别。<|endoftext|>Human Rights Watch is a non-profit organization."),
        "离别。"
    );
}

#[test]
fn strip_meta_instructions_drops_prompt_hallucinations() {
    // 实测：歌曲尾部窗口上 Qwen3-ASR 吐出训练指令而非歌词。
    assert_eq!(
        strip_meta_instructions("特别注意，不要把“system”翻译成“系统”。"),
        ""
    );
    assert_eq!(
        strip_meta_instructions("不要把\"system\"翻译成\"系统\""),
        ""
    );
    assert_eq!(strip_meta_instructions("请把这段音频转写出来。"), "");
    // 同段里真实歌词保留，只丢元指令句。
    assert_eq!(
        strip_meta_instructions("回头看，不曾走远。特别注意，不要把“system”翻译成“系统”。"),
        "回头看，不曾走远。"
    );
    // 歌词里的「翻译 / 请」不带指令特征时不能误伤。
    assert_eq!(
        strip_meta_instructions("把思念翻译成一首歌。"),
        "把思念翻译成一首歌。"
    );
    assert_eq!(strip_meta_instructions("请不要离开。"), "请不要离开。");
}

#[test]
fn resample_16k_is_identity() {
    let pcm: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.01).sin()).collect();
    assert_eq!(resample_to_asr(&pcm, ASR_SAMPLE_RATE).unwrap(), pcm);
}

#[test]
fn resample_lengths_follow_ratio() {
    let pcm = vec![0.0f32; 48_000];
    assert_eq!(resample_to_asr(&pcm, 48_000).unwrap().len(), 16_000);
    assert_eq!(resample_to_asr(&pcm, 8_000).unwrap().len(), 96_000);
    // 44.1 kHz 的 1 秒音频重采样后时长仍为 1 秒，时间轴不会漂移。
    let one_second = vec![0.0f32; 44_100];
    let out = resample_to_asr(&one_second, 44_100).unwrap();
    assert!((out.len() as f64 / ASR_SAMPLE_RATE as f64 - 1.0).abs() < 1e-3);
    assert!(resample_to_asr(&[], 44_100).unwrap().is_empty());
}

#[test]
fn resample_preserves_dc_gain() {
    for rate in [8_000, 44_100, 48_000] {
        let pcm = vec![0.25f32; rate as usize];
        let out = resample_to_asr(&pcm, rate).unwrap();
        let max_err = out
            .iter()
            .map(|value| (value - 0.25).abs())
            .fold(0.0f32, f32::max);
        assert!(max_err < 1e-4, "rate={rate} 直流增益偏差过大：{max_err}");
    }
}

#[test]
fn resample_suppresses_out_of_band_on_downsample() {
    fn rms(signal: &[f32]) -> f64 {
        let sum: f64 = signal.iter().map(|v| (*v as f64).powi(2)).sum();
        (sum / signal.len().max(1) as f64).sqrt()
    }
    let rate = 48_000usize;
    let tone = |freq: f64| -> Vec<f32> {
        (0..rate)
            .map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).sin() as f32)
            .collect()
    };
    // 1 kHz 在目标 16 kHz 的带内，应基本无损；12 kHz 高于目标 Nyquist(8 kHz)，
    // 若不做带限会混叠成 4 kHz 的假信号，必须被明显衰减。
    let in_band = rms(&resample_to_asr(&tone(1_000.0), rate as i32).unwrap());
    let out_band = rms(&resample_to_asr(&tone(12_000.0), rate as i32).unwrap());
    assert!(in_band > 0.5, "带内 1 kHz 应基本无损，实测 {in_band}");
    assert!(
        out_band < in_band * 0.25,
        "带外 12 kHz 未被抑制：{out_band} vs {in_band}"
    );
}

#[test]
fn resample_rejects_invalid_rate() {
    assert!(resample_to_asr(&[0.1, 0.2], 0).is_err());
    assert!(resample_to_asr(&[0.1, 0.2], -1).is_err());
}

/// 走一遍真实的 WAV 读写链路：44.1 kHz WAV → `Wave::read` → 兜底重采样。
#[test]
fn resample_real_wav_44100_to_16k() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.wav");
    let pcm: Vec<f32> = (0..44_100)
        .map(|i| (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 44_100.0).sin() as f32)
        .collect();
    assert!(sherpa_onnx::write(path.to_str().unwrap(), &pcm, 44_100));

    let wave = Wave::read(path.to_str().unwrap()).expect("应能读回 44.1 kHz 单声道 WAV");
    assert_eq!(wave.sample_rate(), 44_100);
    let out = resample_to_asr(wave.samples(), wave.sample_rate()).unwrap();
    assert_eq!(out.len(), 16_000, "重采样后应为 1 秒 16 kHz 音频");

    // 440 Hz 处于目标带内，重采样后能量应基本保留（正弦 RMS ≈ 1/√2）。
    let rms = (out.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / out.len() as f64).sqrt();
    assert!(
        (rms - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.05,
        "带内 440 Hz 能量损失过大：rms={rms}"
    );
}

fn tone_with_gaps(seconds: f64, gaps: &[(f64, f64)]) -> Vec<f32> {
    let rate = ASR_SAMPLE_RATE as usize;
    (0..(rate as f64 * seconds) as usize)
        .map(|index| {
            let time = index as f64 / rate as f64;
            if gaps.iter().any(|(from, to)| (*from..*to).contains(&time)) {
                0.0
            } else {
                (2.0 * std::f64::consts::PI * 440.0 * time).sin() as f32 * 0.2
            }
        })
        .collect()
}

#[test]
fn plan_segments_snaps_to_quiet_point() {
    let rate = ASR_SAMPLE_RATE as usize;
    let total = 20.0;
    let pcm = tone_with_gaps(total, &[(9.0, 9.8)]);
    let ranges = plan_segments(&pcm, rate, total, SEGMENT_TARGET_SECS);
    assert_eq!(ranges.len(), 2, "20 秒音频应切成两段：{ranges:?}");
    assert!((ranges[0].0).abs() < 1e-9);
    assert!(
        (9.0..=9.8).contains(&ranges[0].1),
        "切点应落在静音段内，实测 {}",
        ranges[0].1
    );
    assert!((ranges[1].0 - ranges[0].1).abs() < 1e-9);
    assert!((ranges[1].1 - total).abs() < 1e-9);
}

#[test]
fn plan_segments_cover_audio_without_gaps() {
    let rate = ASR_SAMPLE_RATE as usize;
    let total = 31.0;
    let pcm = tone_with_gaps(total, &[]);
    let ranges = plan_segments(&pcm, rate, total, SEGMENT_TARGET_SECS);
    assert!(ranges.len() >= 3, "31 秒音频至少切成三段：{ranges:?}");
    assert!((ranges[0].0).abs() < 1e-9);
    for pair in ranges.windows(2) {
        assert!(
            (pair[0].1 - pair[1].0).abs() < 1e-9,
            "分片必须首尾相接：{ranges:?}"
        );
    }
    let last = ranges.last().unwrap();
    assert!((last.1 - total).abs() < 1e-9, "最后一片应覆盖到音频结尾");
    for (start, end) in &ranges {
        assert!(
            *end - *start >= SEGMENT_MIN_SECS - 1e-9,
            "分片过短：{start}..{end}"
        );
        assert!(
            *end - *start <= SEGMENT_TARGET_SECS * 1.5 + 1e-9,
            "分片过长：{start}..{end}"
        );
    }
}

#[test]
fn plan_segments_short_audio_is_single_range() {
    let rate = ASR_SAMPLE_RATE as usize;
    let total = 4.0;
    let pcm = tone_with_gaps(total, &[]);
    let ranges = plan_segments(&pcm, rate, total, SEGMENT_TARGET_SECS);
    assert_eq!(ranges.len(), 1);
    assert!((ranges[0].0).abs() < 1e-9);
    assert!((ranges[0].1 - total).abs() < 1e-9);
    assert!(plan_segments(&[], rate, 0.0, SEGMENT_TARGET_SECS).is_empty());
}
