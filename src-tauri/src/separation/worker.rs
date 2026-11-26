//! 独立子进程里的分离任务：解码一次，两个引擎各写一条音轨。
//!
//! sherpa-onnx 遇到不兼容模型会直接 `_Exit(-1)`，无法在主进程内捕获；
//! 把引擎调用隔离到子进程后，最坏情况只是 worker 退出，主进程照常报错。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use super::decode::{decode_input, tail_text};
use super::ffi::with_engine;
use super::{INPUT_SAMPLE_RATE, Source, StemTarget, WORKER_ARG, write_wav16_slices};

/// 超过该时长的音频改为串行分离：两个引擎的整轨输出不同时驻留，峰值内存约减半。
const SEQUENTIAL_SEPARATION_SECONDS: usize = 10 * 60;

/// 主进程写给 worker 的任务描述；worker 只做解码、分离与写盘，不接触 Tauri 状态。
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WorkerJob {
    pub(super) source_path: String,
    /// 用户选择的人声模型。
    pub(super) vocals: WorkerEngine,
    pub(super) vocals_target: String,
    /// 固定伴奏模型（UVR-MDX-NET-Inst_HQ_3）。
    pub(super) accompaniment: WorkerEngine,
    pub(super) accompaniment_target: String,
    pub(super) vocals_path: String,
    pub(super) accompaniment_path: String,
    pub(super) result_path: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub(super) enum WorkerEngine {
    Spleeter {
        vocals: String,
        accompaniment: String,
    },
    Uvr {
        model: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WorkerResult {
    pub(super) ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) sample_rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) error: Option<String>,
}

/// 以 worker 参数启动时执行分离任务；返回 `None` 表示当前进程不是 worker。
pub fn worker_main() -> Option<i32> {
    let mut args = std::env::args_os();
    let _exe = args.next();
    if args.next().as_deref() != Some(std::ffi::OsStr::new(WORKER_ARG)) {
        return None;
    }
    let Some(job_path) = args.next().map(PathBuf::from) else {
        return Some(2);
    };
    Some(run_worker(&job_path))
}

pub(super) fn run_worker(job_path: &Path) -> i32 {
    let Ok(bytes) = fs::read(job_path) else {
        return 2;
    };
    let Ok(job) = serde_json::from_slice::<WorkerJob>(&bytes) else {
        return 2;
    };
    let result = execute_job(&job);
    let payload = match &result {
        Ok(sample_rate) => WorkerResult {
            ok: true,
            sample_rate: Some(*sample_rate),
            error: None,
        },
        Err(error) => WorkerResult {
            ok: false,
            sample_rate: None,
            error: Some(format!("{error:#}")),
        },
    };
    let result_path = Path::new(&job.result_path);
    let _ = fs::write(
        result_path,
        serde_json::to_vec(&payload).unwrap_or_default(),
    );
    if result.is_ok() { 0 } else { 1 }
}

pub(super) fn worker_engine(source: &Source) -> WorkerEngine {
    match source {
        Source::Spleeter {
            vocals,
            accompaniment,
        } => WorkerEngine::Spleeter {
            vocals: vocals.to_string_lossy().into_owned(),
            accompaniment: accompaniment.to_string_lossy().into_owned(),
        },
        Source::Uvr { model } => WorkerEngine::Uvr {
            model: model.to_string_lossy().into_owned(),
        },
    }
}

fn worker_source(engine: &WorkerEngine) -> Source {
    match engine {
        WorkerEngine::Spleeter {
            vocals,
            accompaniment,
        } => Source::Spleeter {
            vocals: PathBuf::from(vocals),
            accompaniment: PathBuf::from(accompaniment),
        },
        WorkerEngine::Uvr { model } => Source::Uvr {
            model: PathBuf::from(model),
        },
    }
}

fn parse_target(value: &str) -> Result<StemTarget> {
    match value {
        "vocals" => Ok(StemTarget::Vocals),
        "instrumental" => Ok(StemTarget::Instrumental),
        other => bail!("未知的分离主输出：{other}"),
    }
}

/// 在引擎输出的两个 stem 中，找到指定角色所在的下标。
///
/// sherpa-onnx 只保证 `stems[0]` = 模型主输出、`stems[1]` = 「混合 − 主输出」。
/// 「想要的角色」与「模型主输出角色」一致时取 0，否则取 1：
/// - 人声模型取人声 → 0；人声模型取伴奏 → 1；
/// - 乐器模型取乐器 → 0；乐器模型取人声 → 1。
///
/// 这与旧版单引擎的 `Instrumental => (人声 1, 伴奏 0)` 映射一致；固定伴奏模型
/// 是乐器模型，因此伴奏取它自己的 `stems[0]`（不是角色枚举值本身）。
pub(super) fn stem_index(primary: StemTarget, wanted: StemTarget) -> usize {
    if primary == wanted { 0 } else { 1 }
}

/// 取出线程 panic 的文本，方便在子进程日志里定位。
fn panic_payload(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "未知 panic".to_owned())
}

fn execute_job(job: &WorkerJob) -> Result<u32> {
    let vocals_source = worker_source(&job.vocals);
    let accompaniment_source = worker_source(&job.accompaniment);
    let vocals_target = parse_target(&job.vocals_target)?;
    let accompaniment_target = parse_target(&job.accompaniment_target)?;
    let input = decode_input(Path::new(&job.source_path))?;
    let vocals_path = PathBuf::from(&job.vocals_path);
    let accompaniment_path = PathBuf::from(&job.accompaniment_path);
    let vocals_index = stem_index(vocals_target, StemTarget::Vocals);
    let accompaniment_index = stem_index(accompaniment_target, StemTarget::Instrumental);

    let run_vocals = |threads: i32| -> Result<u32> {
        with_engine(&vocals_source, &input, INPUT_SAMPLE_RATE, threads, |raw| {
            if raw.stems.len() < 2 {
                bail!("人声分离失败：所选模型未同时输出人声轨与残差轨");
            }
            let stem = &raw.stems[vocals_index];
            if stem.is_empty() || stem[0].is_empty() {
                bail!("人声分离失败：人声轨为空");
            }
            write_wav16_slices(&vocals_path, stem, raw.sample_rate)?;
            Ok(raw.sample_rate)
        })
    };
    let run_accompaniment = |threads: i32| -> Result<()> {
        with_engine(
            &accompaniment_source,
            &input,
            INPUT_SAMPLE_RATE,
            threads,
            |raw| {
                if raw.stems.len() < 2 {
                    bail!("伴奏分离失败：伴奏模型未同时输出乐器轨与残差轨");
                }
                let stem = &raw.stems[accompaniment_index];
                if stem.is_empty() || stem[0].is_empty() {
                    bail!("伴奏分离失败：伴奏轨为空");
                }
                write_wav16_slices(&accompaniment_path, stem, raw.sample_rate)?;
                Ok(())
            },
        )
    };

    let cores = available_cores();
    // 超长音频串行执行：两个引擎的整轨输出不同时驻留，避免峰值内存翻倍。
    let samples = input.first().map_or(0, Vec::len);
    if samples > SEQUENTIAL_SEPARATION_SECONDS * INPUT_SAMPLE_RATE as usize {
        let threads = split_thread_count(cores, 1);
        let sample_rate = run_vocals(threads)?;
        run_accompaniment(threads)?;
        return Ok(sample_rate);
    }

    // 常规歌曲：解码只做一次，两个独立引擎并行生成，线程数按核心均分。
    let threads = split_thread_count(cores, 2);
    let (vocals_result, accompaniment_result) = std::thread::scope(|scope| {
        let vocals = scope.spawn(|| run_vocals(threads));
        let accompaniment = scope.spawn(|| run_accompaniment(threads));
        (vocals.join(), accompaniment.join())
    });
    let sample_rate = vocals_result.map_err(|payload| {
        anyhow!("人声分离线程异常结束：{}", panic_payload(payload.as_ref()))
    })??;
    accompaniment_result.map_err(|payload| {
        anyhow!("伴奏分离线程异常结束：{}", panic_payload(payload.as_ref()))
    })??;
    Ok(sample_rate)
}

pub(super) fn run_worker_process(job_path: &Path) -> Result<std::process::Output> {
    let exe = std::env::current_exe().context("无法定位应用可执行文件")?;
    let mut command = bgm_core::process::command(&exe);
    command
        .arg(WORKER_ARG)
        .arg(job_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    command
        .output()
        .with_context(|| format!("无法启动人声分离子进程：{}", exe.display()))
}

/// 子进程异常退出时的诊断信息：退出码 + sherpa-onnx 在 stderr 留下的最后一段日志。
pub(super) fn worker_failure_detail(output: &std::process::Output) -> String {
    let status = describe_status(&output.status);
    let tail = tail_text(&output.stderr, "").trim().to_owned();
    if tail.is_empty() {
        status
    } else {
        format!("{status}；{tail}")
    }
}

fn describe_status(status: &std::process::ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("退出码 {code}"),
        None => "被异常终止".to_owned(),
    }
}

/// 可用 CPU 核心数；探测失败时按 2 核处理。
pub(super) fn available_cores() -> usize {
    std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(2)
}

/// 并行引擎各自分到的线程数：按引擎数均分核心，避免两个模型互相抢核。
pub(super) fn split_thread_count(cores: usize, engines: usize) -> i32 {
    (cores / engines.max(1)).clamp(1, 4) as i32
}
