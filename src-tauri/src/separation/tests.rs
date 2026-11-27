//! 分离模块的单元测试与可选的端到端验证。

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use super::decode::{decode_input, read_decoded};
use super::ffi::run_engine;
use super::models::{extract_archive, preflight_custom, spleeter_files, uvr_target_from_name};
use super::worker::{available_cores, run_worker, split_thread_count, stem_index};
use super::*;

#[test]
fn uvr_target_uses_inst_in_name() {
    assert_eq!(
        uvr_target_from_name("UVR-MDX-NET-Inst_HQ_3"),
        StemTarget::Instrumental
    );
    assert_eq!(
        uvr_target_from_name("UVR-MDX-NET-Voc_FT"),
        StemTarget::Vocals
    );
    assert_eq!(uvr_target_from_name("custom_model"), StemTarget::Vocals);
}

/// 回归用例：下标由「模型主输出角色」与「想取的角色」共同决定。
///
/// sherpa-onnx 只保证 `stems[0]` 是模型主输出。人声模型的主输出是人声，
/// 乐器模型（含固定伴奏模型）的主输出是乐器；两者取到「伴奏」时下标分别是 1 和 0。
/// 该映射与旧版单引擎的 `Instrumental => (人声 1, 伴奏 0)` 完全一致。
#[test]
fn stem_index_follows_primary_output() {
    assert_eq!(stem_index(StemTarget::Vocals, StemTarget::Vocals), 0);
    assert_eq!(stem_index(StemTarget::Vocals, StemTarget::Instrumental), 1);
    assert_eq!(
        stem_index(StemTarget::Instrumental, StemTarget::Instrumental),
        0
    );
    assert_eq!(stem_index(StemTarget::Instrumental, StemTarget::Vocals), 1);
}

#[test]
fn split_thread_count_shares_cores_between_engines() {
    assert_eq!(split_thread_count(12, 2), 4);
    assert_eq!(split_thread_count(8, 2), 4);
    assert_eq!(split_thread_count(4, 2), 2);
    assert_eq!(split_thread_count(2, 2), 1);
    assert_eq!(split_thread_count(1, 2), 1);
    assert_eq!(split_thread_count(0, 2), 1);
    assert_eq!(split_thread_count(16, 1), 4);
}

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(relative)
}

/// 真实链路验证：Spleeter 分离 → 保存人声/伴奏 → 只对 vocals 做 ASR。
///
/// 需要仓库内随包模型、ffmpeg、真实歌曲与已下载的 ASR 模型；默认不执行。
/// 可用 `TFY_E2E_SONG` / `TFY_ASR_MODEL_DIR` 覆盖输入与模型位置。
#[test]
#[ignore = "需要真实歌曲、随包分离模型与 ASR 模型"]
fn spleeter_then_asr_only_on_vocals() {
    let song = std::env::var_os("TFY_E2E_SONG")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_path("bgm/cheerful/上春山.mp3"));
    if !song.is_file() {
        eprintln!("跳过：找不到测试歌曲 {}", song.display());
        return;
    }
    let archive = repo_path(&format!("models/{SPLEETER_ARCHIVE}"));
    assert!(archive.is_file(), "缺少随包模型：{}", archive.display());
    let temp = tempfile::tempdir().unwrap();
    extract_archive(&archive, temp.path()).unwrap();
    let (vocals, accompaniment) = spleeter_files(temp.path()).expect("解压后应有双模型");
    let input = decode_input(&song).unwrap();
    assert_eq!(input.len(), 2);

    let output = run_engine(
        &Source::Spleeter {
            vocals,
            accompaniment,
        },
        &input,
        44_100,
        split_thread_count(available_cores(), 1),
    )
    .unwrap();
    assert_eq!(output.stems.len(), 2, "Spleeter 应输出两轨");
    assert!(!output.stems[0][0].is_empty());
    assert!(!output.stems[1][0].is_empty());

    let vocals_path = temp.path().join("vocals.wav");
    let accompaniment_path = temp.path().join("accompaniment.wav");
    let vocals_size = write_wav16(&vocals_path, &output.stems[0], output.sample_rate).unwrap();
    let accompaniment_size =
        write_wav16(&accompaniment_path, &output.stems[1], output.sample_rate).unwrap();
    assert!(vocals_size > 44);
    assert!(accompaniment_size > 44);
    eprintln!("人声文件：{}（{vocals_size} 字节）", vocals_path.display());
    eprintln!(
        "伴奏文件：{}（{accompaniment_size} 字节）",
        accompaniment_path.display()
    );

    let asr_wav = temp.path().join("vocals-16k.wav");
    let ffmpeg = bgm_core::loudness::ffmpeg_path().expect("需要 ffmpeg");
    let status = bgm_core::process::quiet_command(&ffmpeg)
        .args(["-hide_banner", "-nostdin", "-y", "-i"])
        .arg(&vocals_path)
        .args(["-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
        .arg(&asr_wav)
        .output()
        .expect("启动 ffmpeg 失败");
    assert!(status.status.success(), "归一化人声轨失败");

    let model_dir = std::env::var_os("TFY_ASR_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::download_dir()
                .unwrap_or_default()
                .join("bgm-models/sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25")
        });
    if !model_dir.is_dir() {
        eprintln!("跳过 ASR：找不到模型 {}", model_dir.display());
        return;
    }
    let runtime = crate::models::ModelRuntime::default();
    runtime
        .load(model_dir.parent().unwrap(), &model_dir, |_| {})
        .unwrap();
    let segments = runtime
        .transcribe_segments(
            &asr_wav,
            10.0,
            &tokio_util::sync::CancellationToken::new(),
            |_| {},
            &mut |_, _| {},
        )
        .unwrap();
    assert!(!segments.is_empty(), "人声轨应能识别出歌词");
    let text = segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join(" / ");
    eprintln!("仅人声轨识别结果：{text}");
}

/// 两个随包 UVR 单模型都能被 C API 加载并输出两条非空音轨。
#[test]
#[ignore = "需要随包 UVR 模型、ffmpeg 与真实歌曲"]
fn uvr_models_run_on_short_clip() {
    let song = std::env::var_os("TFY_E2E_SONG")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_path("bgm/cheerful/上春山.mp3"));
    if !song.is_file() {
        eprintln!("跳过：找不到测试歌曲 {}", song.display());
        return;
    }
    let mut input = decode_input(&song).unwrap();
    // 只取前 10 秒，控制验证耗时；分离引擎本身支持整首。
    for channel in &mut input {
        channel.truncate(10 * INPUT_SAMPLE_RATE as usize);
    }
    for file in ["UVR-MDX-NET-Voc_FT.onnx", "UVR-MDX-NET-Inst_HQ_3.onnx"] {
        let model = repo_path(&format!("models/{file}"));
        assert!(model.is_file(), "缺少随包模型：{}", model.display());
        let output = run_engine(
            &Source::Uvr { model },
            &input,
            INPUT_SAMPLE_RATE,
            split_thread_count(available_cores(), 1),
        )
        .unwrap_or_else(|error| panic!("{file} 分离失败：{error:#}"));
        assert_eq!(output.stems.len(), 2, "{file} 应输出两轨");
        assert!(!output.stems[0][0].is_empty(), "{file} 主轨为空");
        assert!(!output.stems[1][0].is_empty(), "{file} 残差轨为空");
        eprintln!(
            "{file} 输出 {} 轨 @ {} Hz",
            output.stems.len(),
            output.sample_rate
        );
    }
}

/// 两个 UVR 引擎在独立线程中并发运行，验证 C API / ORT 会话可并行。
#[test]
#[ignore = "需要随包 UVR 模型、ffmpeg 与真实歌曲"]
fn parallel_uvr_engines_run_on_short_clip() {
    let song = std::env::var_os("TFY_E2E_SONG")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_path("bgm/cheerful/上春山.mp3"));
    if !song.is_file() {
        eprintln!("跳过：找不到测试歌曲 {}", song.display());
        return;
    }
    let mut input = decode_input(&song).unwrap();
    for channel in &mut input {
        channel.truncate(10 * INPUT_SAMPLE_RATE as usize);
    }
    let vocals_model = repo_path("models/UVR-MDX-NET-Voc_FT.onnx");
    let accompaniment_model = repo_path("models/UVR-MDX-NET-Inst_HQ_3.onnx");
    assert!(
        vocals_model.is_file(),
        "缺少随包模型：{}",
        vocals_model.display()
    );
    assert!(
        accompaniment_model.is_file(),
        "缺少随包模型：{}",
        accompaniment_model.display()
    );
    let threads = split_thread_count(available_cores(), 2);
    let input = &input;
    let (vocals, accompaniment) = std::thread::scope(|scope| {
        let vocals = scope.spawn(move || {
            run_engine(
                &Source::Uvr {
                    model: vocals_model,
                },
                input,
                INPUT_SAMPLE_RATE,
                threads,
            )
        });
        let accompaniment = scope.spawn(move || {
            run_engine(
                &Source::Uvr {
                    model: accompaniment_model,
                },
                input,
                INPUT_SAMPLE_RATE,
                threads,
            )
        });
        (vocals.join().unwrap(), accompaniment.join().unwrap())
    });
    let vocals = vocals.expect("人声模型并行运行失败");
    let accompaniment = accompaniment.expect("伴奏模型并行运行失败");
    assert_eq!(vocals.stems.len(), 2);
    assert_eq!(accompaniment.stems.len(), 2);
    assert!(!vocals.stems[0][0].is_empty());
    assert!(!accompaniment.stems[0][0].is_empty());
    eprintln!(
        "并行分离完成：人声 {} 轨 / 伴奏 {} 轨",
        vocals.stems.len(),
        accompaniment.stems.len()
    );
}

#[test]
fn wav_header_is_valid_pcm16() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vocals.wav");
    let left = vec![0.0f32, 0.5, -0.5];
    let right = vec![0.1f32, -0.1, 0.0];
    let size = write_wav16(&path, &[left, right], 44_100).unwrap();
    let bytes = fs::read(&path).unwrap();
    assert_eq!(size as usize, bytes.len());
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    assert_eq!(u16::from_le_bytes([bytes[22], bytes[23]]), 2);
    assert_eq!(
        u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]),
        44_100
    );
    assert_eq!(&bytes[36..40], b"data");
    assert_eq!(bytes.len(), 44 + 3 * 2 * 2);
}

fn write_tarball(path: &Path, entries: &[(&str, &[u8])]) {
    let file = File::create(path).unwrap();
    let encoder = bzip2::write::BzEncoder::new(file, bzip2::Compression::best());
    let mut archive = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    for (name, data) in entries {
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive.append_data(&mut header, name, *data).unwrap();
    }
    archive.into_inner().unwrap().finish().unwrap();
}

#[test]
fn extract_archive_installs_models_and_replaces_stale_target() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join(SPLEETER_ARCHIVE);
    let vocals = format!("{SPLEETER_DIR}/vocals.int8.onnx");
    let accompaniment = format!("{SPLEETER_DIR}/accompaniment.int8.onnx");
    write_tarball(
        &archive,
        &[
            (vocals.as_str(), b"vocals"),
            (accompaniment.as_str(), b"accompaniment"),
        ],
    );

    let target = dir.path().join("installed");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("stale.txt"), b"old").unwrap();

    extract_archive(&archive, &target).unwrap();
    assert!(spleeter_files(&target).is_some(), "应解压出双模型");
    assert!(!target.join("stale.txt").exists(), "旧内容应被替换");

    // 已就绪时再次调用应为幂等操作。
    extract_archive(&archive, &target).unwrap();
    assert!(spleeter_files(&target).is_some());
}

#[test]
fn extract_archive_rejects_archive_without_models() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join(SPLEETER_ARCHIVE);
    write_tarball(&archive, &[("README.txt", b"no models here")]);
    let target = dir.path().join("installed");
    let error = extract_archive(&archive, &target).unwrap_err();
    assert!(error.to_string().contains("未找到"), "{error}");
    assert!(!target.exists(), "失败时不应留下目标目录");
}

fn metadata_bytes(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (key, value) in entries {
        bytes.push(0x0a);
        bytes.push(key.len() as u8);
        bytes.extend_from_slice(key.as_bytes());
        bytes.push(0x12);
        bytes.push(value.len() as u8);
        bytes.extend_from_slice(value.as_bytes());
    }
    bytes
}

fn complete_uvr_metadata(model_type: &str, stems: &str) -> Vec<u8> {
    let entries = vec![
        ("model_type", model_type),
        ("stems", stems),
        ("sample_rate", "44100"),
        ("n_fft", "6144"),
        ("center", "1"),
        ("win_length", "6144"),
        ("hop_length", "1024"),
        ("dim_t", "256"),
        ("dim_f", "3072"),
        ("dim_c", "4"),
        ("window_type", "hann"),
    ];
    metadata_bytes(&entries)
}

#[test]
fn preflight_accepts_complete_uvr_model() {
    let dir = tempfile::tempdir().unwrap();
    let model = dir.path().join("model.onnx");
    fs::write(&model, complete_uvr_metadata("UVR", "2")).unwrap();
    preflight_custom(&Source::Uvr { model }).unwrap();
}

#[test]
fn preflight_rejects_wrong_type_stem_count_and_missing_keys() {
    let dir = tempfile::tempdir().unwrap();
    let model = dir.path().join("model.onnx");

    fs::write(&model, complete_uvr_metadata("spleeter", "2")).unwrap();
    let error = preflight_custom(&Source::Uvr {
        model: model.clone(),
    })
    .unwrap_err();
    assert!(error.to_string().contains("model_type"), "{error}");

    fs::write(&model, complete_uvr_metadata("UVR", "4")).unwrap();
    let error = preflight_custom(&Source::Uvr {
        model: model.clone(),
    })
    .unwrap_err();
    assert!(error.to_string().contains("2-stem"), "{error}");

    let missing = metadata_bytes(&[
        ("model_type", "UVR"),
        ("stems", "2"),
        ("sample_rate", "44100"),
    ]);
    fs::write(&model, missing).unwrap();
    let error = preflight_custom(&Source::Uvr { model }).unwrap_err();
    assert!(error.to_string().contains("缺少"), "{error}");
    assert!(error.to_string().contains("元数据"), "{error}");
}

#[test]
fn read_decoded_splits_interleaved_stereo() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("decoded.f32");
    let mut bytes = Vec::new();
    for sample in [0.0f32, 1.0, 0.5, -0.5, -1.0, 0.25] {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    fs::write(&raw, bytes).unwrap();
    let channels = read_decoded(&raw).unwrap();
    assert_eq!(channels.len(), 2);
    assert_eq!(channels[0], vec![0.0, 0.5, -1.0]);
    assert_eq!(channels[1], vec![1.0, -0.5, 0.25]);
}

#[test]
fn worker_rejects_malformed_job() {
    let dir = tempfile::tempdir().unwrap();
    let job = dir.path().join("job.json");
    fs::write(&job, b"not json").unwrap();
    assert_eq!(run_worker(&job), 2);
}

/// 用真实模型验证 stem 语义：伴奏模型的主输出（stem[0]）是乐器，不是人声。
///
/// 以人声模型 stem[0] 为参照，乐器模型的 stem[1] 应与之高度相关（残差 = 人声），
/// 而 stem[0] 几乎不相关（乐器）。若有人把「伴奏 → 下标」写成 1，
/// 本测试会失败，提醒伴奏轨会被写成人声。
#[test]
#[ignore = "需要随包 UVR 模型、ffmpeg 与真实歌曲"]
fn accompaniment_model_primary_stem_is_instrumental() {
    fn correlation(a: &[f32], b: &[f32]) -> f32 {
        let n = a.len().min(b.len());
        let (mut mean_a, mut mean_b) = (0f64, 0f64);
        for i in 0..n {
            mean_a += a[i] as f64;
            mean_b += b[i] as f64;
        }
        mean_a /= n as f64;
        mean_b /= n as f64;
        let (mut num, mut dev_a, mut dev_b) = (0f64, 0f64, 0f64);
        for i in 0..n {
            let x = a[i] as f64 - mean_a;
            let y = b[i] as f64 - mean_b;
            num += x * y;
            dev_a += x * x;
            dev_b += y * y;
        }
        (num / (dev_a.sqrt() * dev_b.sqrt() + 1e-12)) as f32
    }

    let song = repo_path("bgm/cheerful/buttercup.mp3");
    let mut input = decode_input(&song).unwrap();
    for channel in &mut input {
        channel.truncate(10 * INPUT_SAMPLE_RATE as usize);
    }
    let threads = split_thread_count(available_cores(), 1);
    let vocals = run_engine(
        &Source::Uvr {
            model: repo_path("models/UVR-MDX-NET-Voc_FT.onnx"),
        },
        &input,
        INPUT_SAMPLE_RATE,
        threads,
    )
    .unwrap();
    let accompaniment = run_engine(
        &Source::Uvr {
            model: repo_path(&format!("models/{ACCOMPANIMENT_MODEL_FILE}")),
        },
        &input,
        INPUT_SAMPLE_RATE,
        threads,
    )
    .unwrap();

    let vocals_ch0 = &vocals.stems[0][0];
    let with_primary = correlation(vocals_ch0, &accompaniment.stems[0][0]);
    let with_residual = correlation(vocals_ch0, &accompaniment.stems[1][0]);
    assert!(
        with_residual > with_primary + 0.3,
        "伴奏模型的 stem[1]（残差）应是人声：与主输出的相关度 {with_residual} 未显著高于 {with_primary}"
    );
}
