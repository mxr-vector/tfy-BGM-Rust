//! Headless benchmarks for the production speech runtimes.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sherpa_onnx::Wave;

use crate::models::{ModelRuntime, SEGMENT_TARGET_SECS};

const USAGE: &str = "\
Usage:
  tfy-bgm --speech-bench asr MODEL_DIR INPUT.wav [WARMUP [RUNS [JSON]]]
  tfy-bgm --speech-bench asr-whole MODEL_DIR INPUT.wav [WARMUP [RUNS [JSON]]]
  tfy-bgm --speech-bench tts ROOT MODEL_ID CACHE TEXTS_FILE OUTPUT
      [WARMUP [RUNS [JSON [PROMPT.wav PROMPT_TEXT]]]]
  tfy-bgm --speech-bench --help

WARMUP defaults to 1 (0 is allowed); RUNS defaults to 5 and must be positive.
JSON is a report path; omit it or pass '-' for stdout. Use a report path on
Windows if the GUI-subsystem executable has no attached stdout.
ASR uses a mono WAV and 10-second target windows; asr-whole decodes it as one clip.
TTS reads one utterance per non-blank UTF-8 line in TEXTS_FILE. OUTPUT is the
TTS audio output path. Quote PROMPT_TEXT and pass both prompt arguments or neither.";

/// Returns `None` for normal application startup; otherwise returns the CLI exit code.
pub fn main() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    args.find(|arg| arg == "--speech-bench")?;
    let args: Vec<_> = args.collect();
    if matches!(args.as_slice(), [arg] if arg == "--help" || arg == "-h") {
        return Some(match writeln!(io::stdout().lock(), "{USAGE}") {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("speech-bench: {error}");
                1
            }
        });
    }

    let (job, options) = match parse(&args) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("speech-bench: {error:#}\n\n{USAGE}");
            return Some(2);
        }
    };
    Some(
        match run(job, &options).and_then(|report| write_report(options.json, &report)) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("speech-bench: {error:#}");
                1
            }
        },
    )
}

#[derive(Clone, Copy)]
enum AsrMode {
    Segments,
    Whole,
}

enum Job<'a> {
    Asr {
        dir: &'a Path,
        wav: &'a Path,
        mode: AsrMode,
    },
    Tts {
        root: &'a Path,
        id: &'a str,
        cache: &'a Path,
        texts: &'a Path,
        output: &'a Path,
        prompt: Option<(&'a Path, &'a str)>,
    },
}

struct Options<'a> {
    warmup: usize,
    runs: usize,
    json: Option<&'a Path>,
}

fn parse(args: &[OsString]) -> Result<(Job<'_>, Options<'_>)> {
    let Some((command, args)) = args.split_first() else {
        bail!("expected an asr, asr-whole, or tts command");
    };
    let (job, options) = match (command.to_str(), args) {
        (Some("asr" | "asr-whole"), [dir, wav, options @ ..]) if options.len() <= 3 => (
            Job::Asr {
                dir: Path::new(dir),
                wav: Path::new(wav),
                mode: if command == "asr" {
                    AsrMode::Segments
                } else {
                    AsrMode::Whole
                },
            },
            options,
        ),
        (Some("tts"), [root, id, cache, texts, output, options @ ..]) => {
            let prompt = match options.get(3..) {
                None | Some([]) => None,
                Some([wav, text]) => Some((
                    Path::new(wav),
                    text.to_str().context("PROMPT_TEXT must be UTF-8")?,
                )),
                _ => bail!("expected both PROMPT.wav and PROMPT_TEXT after JSON"),
            };
            (
                Job::Tts {
                    root: Path::new(root),
                    id: id.to_str().context("MODEL_ID must be UTF-8")?,
                    cache: Path::new(cache),
                    texts: Path::new(texts),
                    output: Path::new(output),
                    prompt,
                },
                &options[..options.len().min(3)],
            )
        }
        _ => bail!("invalid command or positional arguments"),
    };
    let count = |index: usize, name: &str, default| match options.get(index) {
        None => Ok(default),
        Some(arg) => arg
            .to_str()
            .and_then(|value| value.parse::<usize>().ok())
            .with_context(|| format!("{name} must be a non-negative integer")),
    };
    let warmup = count(0, "WARMUP", 1)?;
    let runs = count(1, "RUNS", 5)?;
    if runs == 0 {
        bail!("RUNS must be positive");
    }
    let json = options.get(2).filter(|arg| *arg != "-").map(Path::new);
    Ok((job, Options { warmup, runs, json }))
}

fn run(job: Job<'_>, options: &Options<'_>) -> Result<Value> {
    match job {
        Job::Asr { dir, wav, mode } => benchmark_asr(dir, wav, mode, options),
        Job::Tts {
            root,
            id,
            cache,
            texts,
            output,
            prompt,
        } => {
            let input = fs::read_to_string(texts)
                .with_context(|| format!("read UTF-8 TTS texts from {}", texts.display()))?;
            let texts: Vec<_> = input
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect();
            if texts.is_empty() {
                bail!("TEXTS_FILE must contain at least one non-blank utterance");
            }
            let report = crate::tts::benchmark(
                root,
                id,
                cache,
                &texts,
                prompt,
                output,
                options.warmup,
                options.runs,
            )
            .with_context(|| format!("benchmark TTS model {id}"))?;
            Ok(json!({"schema_version": 1, "kind": "tts", "tts": report}))
        }
    }
}

fn benchmark_asr(dir: &Path, wav: &Path, mode: AsrMode, options: &Options<'_>) -> Result<Value> {
    let dir = std::path::absolute(dir).context("resolve ASR model directory")?;
    let parent = dir.parent().unwrap_or(&dir);
    let (samples, sample_rate) = {
        let filename = wav.to_str().context("INPUT.wav path must be UTF-8")?;
        let wave =
            Wave::read(filename).with_context(|| format!("read mono WAV {}", wav.display()))?;
        (wave.samples().len(), wave.sample_rate())
    };
    if samples == 0 || sample_rate <= 0 {
        bail!("INPUT.wav must contain audio at a positive sample rate");
    }
    let duration_secs = samples as f64 / f64::from(sample_rate);

    let runtime = ModelRuntime::default();
    let token = tokio_util::sync::CancellationToken::new();
    let start = Instant::now();
    let model = runtime
        .load(parent, &dir, |_| {})
        .with_context(|| format!("load ASR model {}", dir.display()))?;
    let cold_load_ms = elapsed_ms(start);

    // Time the whole production call, including WAV loading and any resampling.
    let infer = || -> Result<(f64, Value)> {
        let start = Instant::now();
        match mode {
            AsrMode::Segments => {
                let segments = runtime.transcribe_segments(
                    wav,
                    SEGMENT_TARGET_SECS,
                    &token,
                    |_| {},
                    &mut |_, _| {},
                )?;
                let ms = elapsed_ms(start);
                Ok((ms, json!({"segments": segments})))
            }
            AsrMode::Whole => {
                let text = runtime.transcribe(wav, &token, |_| {})?;
                let ms = elapsed_ms(start);
                Ok((ms, json!({"text": text})))
            }
        }
    };
    for index in 0..options.warmup {
        infer().with_context(|| format!("ASR warmup {}", index + 1))?;
    }
    let mut samples_ms = Vec::new();
    let mut last_result = None;
    for index in 0..options.runs {
        let (ms, result) = infer().with_context(|| format!("ASR run {}", index + 1))?;
        samples_ms.push(ms);
        last_result = Some(result);
    }
    let mut sorted_ms = samples_ms.clone();
    sorted_ms.sort_unstable_by(f64::total_cmp);
    let p50_ms = nearest_rank(&sorted_ms, 50);
    let p95_ms = nearest_rank(&sorted_ms, 95);

    Ok(json!({
        "schema_version": 1,
        "kind": "asr",
        "device": runtime.loaded_device(),
        "mode": match mode { AsrMode::Segments => "segments", AsrMode::Whole => "whole" },
        "segment_window_secs": matches!(mode, AsrMode::Segments).then_some(SEGMENT_TARGET_SECS),
        "model": model,
        "wav": {
            "path": wav,
            "samples": samples,
            "sample_rate_hz": sample_rate,
            "duration_secs": duration_secs,
        },
        "cold_load_ms": cold_load_ms,
        "warmup": options.warmup,
        "runs": options.runs,
        "warm": {
            "samples_ms": samples_ms,
            "p50_ms": p50_ms,
            "p95_ms": p95_ms,
            "rtf_p50": p50_ms / (1000.0 * duration_secs),
            "rtf_p95": p95_ms / (1000.0 * duration_secs),
        },
        "last_result": last_result,
    }))
}

fn elapsed_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

// Nearest rank: p95 is the slowest sample for fewer than 20 runs.
fn nearest_rank(sorted: &[f64], percentile: usize) -> f64 {
    let index = (sorted.len() * percentile).div_ceil(100) - 1;
    sorted[index]
}

fn write_report(path: Option<&Path>, report: &Value) -> Result<()> {
    let mut json = serde_json::to_vec_pretty(report)?;
    json.push(b'\n');
    match path {
        Some(path) => fs::write(path, json)
            .with_context(|| format!("write JSON report to {}", path.display())),
        None => {
            let mut stdout = io::stdout().lock();
            stdout.write_all(&json).context("write JSON to stdout")?;
            stdout.flush().context("flush JSON to stdout")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn asr_defaults_and_invalid_run_count() {
        let input = args(&["asr", "model dir", "speech.wav"]);
        let (job, opts) = parse(&input).unwrap();
        assert!(matches!(
            job,
            Job::Asr {
                mode: AsrMode::Segments,
                ..
            }
        ));
        assert_eq!((opts.warmup, opts.runs, opts.json), (1, 5, None));
        assert!(parse(&args(&["asr", "model", "speech.wav", "0", "0"])).is_err());
    }

    #[test]
    fn tts_prompt_requires_both_arguments() {
        let mut input = args(&[
            "tts", "root", "model", "cache", "texts", "out", "0", "1", "-",
        ]);
        input.push("prompt.wav".into());
        assert!(parse(&input).is_err());
        input.push("A prompt sentence.".into());
        let (job, opts) = parse(&input).unwrap();
        assert!(matches!(
            job,
            Job::Tts {
                prompt: Some((_, "A prompt sentence.")),
                ..
            }
        ));
        assert_eq!((opts.warmup, opts.runs, opts.json), (0, 1, None));
    }

    #[test]
    fn nearest_rank_includes_tail_and_handles_one_sample() {
        assert_eq!(nearest_rank(&[7.0], 95), 7.0);
        assert_eq!(nearest_rank(&[1.0, 2.0, 3.0, 10.0], 50), 2.0);
        assert_eq!(nearest_rank(&[1.0, 2.0, 3.0, 10.0], 95), 10.0);
        let twenty: Vec<_> = (1..=20).map(f64::from).collect();
        assert_eq!(nearest_rank(&twenty, 95), 19.0);
    }
}
