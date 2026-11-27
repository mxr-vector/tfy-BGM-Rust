mod native;

use std::io::{self, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

const WORKER_ARG: &str = "--internal-sherpa-cuda";
const LOAD_TIMEOUT: Duration = Duration::from_secs(60);
const INFER_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_HEADER: usize = 512 * 1024;
const MAX_SAMPLES: usize = 32 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
pub(crate) enum Init {
    Asr {
        dir: PathBuf,
        threads: u32,
    },
    Kokoro {
        model: String,
        voices: String,
        tokens: String,
        lexicon: String,
        threads: i32,
    },
    ZipVoice {
        tokens: String,
        encoder: String,
        decoder: String,
        vocoder: String,
        lexicon: String,
        threads: i32,
    },
}

#[derive(Deserialize, Serialize)]
pub(crate) struct TtsRequest {
    pub speed: f32,
    pub sid: i32,
    pub silence_scale: f32,
    pub reference_sample_rate: i32,
    pub reference_text: Option<String>,
    pub num_steps: i32,
}

#[derive(Deserialize, Serialize)]
enum Request {
    Init(Init),
    Decode,
    Generate { text: String, options: TtsRequest },
}

#[derive(Deserialize, Serialize)]
enum Reply {
    Ready { sample_rate: u32, speakers: i32 },
    Text(String),
    Audio,
    Error(String),
}

fn write_frame<T: Serialize>(out: &mut impl Write, header: &T, samples: &[f32]) -> Result<()> {
    let json = serde_json::to_vec(header)?;
    ensure!(
        json.len() <= MAX_HEADER && samples.len() <= MAX_SAMPLES,
        "speech worker frame too large"
    );
    out.write_all(&(json.len() as u32).to_le_bytes())?;
    out.write_all(&(samples.len() as u32).to_le_bytes())?;
    out.write_all(&json)?;
    for chunk in samples.chunks(1024) {
        let mut bytes = [0; 4096];
        for (dst, sample) in bytes.chunks_exact_mut(4).zip(chunk) {
            dst.copy_from_slice(&sample.to_le_bytes());
        }
        out.write_all(&bytes[..chunk.len() * 4])?;
    }
    out.flush()?;
    Ok(())
}

fn read_frame<T: serde::de::DeserializeOwned>(input: &mut impl Read) -> Result<(T, Vec<f32>)> {
    let mut sizes = [0; 8];
    input.read_exact(&mut sizes)?;
    let header = u32::from_le_bytes(sizes[..4].try_into().unwrap()) as usize;
    let len = u32::from_le_bytes(sizes[4..].try_into().unwrap()) as usize;
    ensure!(
        header <= MAX_HEADER && len <= MAX_SAMPLES,
        "invalid speech worker frame size"
    );
    let mut json = vec![0; header];
    input.read_exact(&mut json)?;
    let header = serde_json::from_slice(&json)?;
    let mut samples = Vec::with_capacity(len);
    let mut bytes = [0; 4096];
    while samples.len() < len {
        let n = ((len - samples.len()) * 4).min(bytes.len());
        input.read_exact(&mut bytes[..n])?;
        samples.extend(
            bytes[..n]
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes(b.try_into().unwrap())),
        );
    }
    Ok((header, samples))
}

pub fn worker_main() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new(WORKER_ARG)) {
        return None;
    }
    let Some(dir) = args.next().map(PathBuf::from) else {
        return Some(2);
    };
    let mut input = BufReader::new(io::stdin().lock());
    let mut output = io::stdout().lock();
    let result = (|| -> Result<()> {
        let (Request::Init(init), _) = read_frame(&mut input)? else {
            bail!("missing model configuration")
        };
        let runtime = native::Native::load(&dir)?;
        let engine = runtime.start(init)?;
        let (sample_rate, speakers) = engine.metadata();
        write_frame(
            &mut output,
            &Reply::Ready {
                sample_rate,
                speakers,
            },
            &[],
        )?;
        loop {
            let (request, samples) = read_frame(&mut input)?;
            match request {
                Request::Decode => {
                    write_frame(&mut output, &Reply::Text(engine.decode(&samples)?), &[])?
                }
                Request::Generate { text, options } => {
                    let audio = engine.generate(&text, &options, &samples)?;
                    write_frame(&mut output, &Reply::Audio, &audio)?;
                }
                Request::Init(_) => bail!("speech worker already initialized"),
            }
        }
    })();
    if let Err(error) = result {
        let _ = write_frame(&mut output, &Reply::Error(format!("{error:#}")), &[]);
    }
    Some(1)
}

pub(crate) struct Worker {
    child: Child,
    requests: Option<SyncSender<(Request, Vec<f32>)>>,
    replies: Option<Receiver<Result<(Reply, Vec<f32>)>>>,
    threads: Vec<JoinHandle<()>>,
    pub sample_rate: u32,
    pub speakers: i32,
}

impl Worker {
    pub fn try_start(
        init: Init,
        threshold: u64,
        token: &CancellationToken,
    ) -> Result<Option<Self>> {
        crate::tts::check_cancelled(token)?;
        if crate::speech_device::force_cpu()
            || !cfg!(all(
                target_arch = "x86_64",
                any(windows, target_os = "linux")
            ))
        {
            return Ok(None);
        }
        let Some(dir) = std::env::var_os("TFY_SHERPA_CUDA_DIR").map(PathBuf::from) else {
            return Ok(None);
        };
        ensure!(
            dir.is_absolute() && dir.is_dir(),
            "TFY_SHERPA_CUDA_DIR must name an absolute runtime directory"
        );
        let Some(device) = crate::speech_device::eligible_nvidia_device(threshold, token)? else {
            eprintln!("[speech] CPU: no NVIDIA device with {threshold} MiB available");
            return Ok(None);
        };
        crate::tts::check_cancelled(token)?;
        let exe = std::env::current_exe()?;
        let mut command = bgm_core::process::command(&exe);
        let loader_var = if cfg!(windows) {
            "PATH"
        } else {
            "LD_LIBRARY_PATH"
        };
        let existing = std::env::var_os(loader_var).unwrap_or_default();
        let paths = std::env::join_paths(
            [dir.clone(), dir.join("lib"), dir.join("bin")]
                .into_iter()
                .chain(std::env::split_paths(&existing))
                .filter(|path| !path.as_os_str().is_empty()),
        )?;
        let mut worker = Self::spawn(
            command
                .arg(WORKER_ARG)
                .arg(&dir)
                .env("CUDA_VISIBLE_DEVICES", &device.uuid)
                .env(loader_var, paths),
        )?;
        let (
            Reply::Ready {
                sample_rate,
                speakers,
            },
            _,
        ) = worker.call(Request::Init(init), &[], LOAD_TIMEOUT, token)?
        else {
            bail!("unexpected Sherpa CUDA startup response");
        };
        ensure!(
            (8000..=65535).contains(&sample_rate),
            "invalid Sherpa CUDA sample rate"
        );
        worker.sample_rate = sample_rate;
        worker.speakers = speakers;
        eprintln!(
            "[speech] Sherpa CUDA ready on {} ({} MiB free at probe)",
            device.uuid, device.free_mib
        );
        Ok(Some(worker))
    }

    fn spawn(command: &mut Command) -> Result<Self> {
        let child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("could not launch isolated Sherpa CUDA worker")?;
        let (send, requests) = mpsc::sync_channel::<(Request, Vec<f32>)>(1);
        let (replies, receive) = mpsc::sync_channel(1);
        let mut worker = Self {
            child,
            requests: Some(send),
            replies: Some(receive),
            threads: Vec::with_capacity(2),
            sample_rate: 0,
            speakers: 0,
        };
        let mut stdin = worker
            .child
            .stdin
            .take()
            .context("missing speech worker stdin")?;
        let mut stdout = BufReader::new(
            worker
                .child
                .stdout
                .take()
                .context("missing speech worker stdout")?,
        );
        let write_errors = replies.clone();
        worker.threads.push(
            thread::Builder::new()
                .name("sherpa-cuda-write".into())
                .spawn(move || {
                    for (request, samples) in requests {
                        if let Err(error) = write_frame(&mut stdin, &request, &samples) {
                            drop(stdin);
                            let _ = write_errors.send(Err(error));
                            break;
                        }
                    }
                })?,
        );
        worker.threads.push(
            thread::Builder::new()
                .name("sherpa-cuda-read".into())
                .spawn(move || {
                    loop {
                        let reply = read_frame(&mut stdout);
                        let done = matches!(&reply, Ok((Reply::Error(_), _)) | Err(_));
                        if replies.send(reply).is_err() || done {
                            break;
                        }
                    }
                })?,
        );
        Ok(worker)
    }

    fn call(
        &self,
        request: Request,
        samples: &[f32],
        timeout: Duration,
        token: &CancellationToken,
    ) -> Result<(Reply, Vec<f32>)> {
        crate::tts::check_cancelled(token)?;
        ensure!(
            samples.len() <= MAX_SAMPLES,
            "speech worker input too large"
        );
        self.requests
            .as_ref()
            .context("speech worker stopped")?
            .try_send((request, samples.to_vec()))
            .map_err(|_| anyhow::anyhow!("speech worker input closed"))?;
        let replies = self.replies.as_ref().context("speech worker stopped")?;
        let deadline = Instant::now() + timeout;
        loop {
            crate::tts::check_cancelled(token)?;
            let left = deadline.saturating_duration_since(Instant::now());
            ensure!(!left.is_zero(), "Sherpa CUDA worker timed out");
            match replies.recv_timeout(left.min(Duration::from_millis(50))) {
                Ok(reply) => {
                    crate::tts::check_cancelled(token)?;
                    return match reply.context("Sherpa CUDA worker pipe closed")? {
                        (Reply::Error(message), _) => Err(anyhow::anyhow!(message)),
                        reply => Ok(reply),
                    };
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => bail!("Sherpa CUDA worker disconnected"),
            }
        }
    }

    pub fn decode(&self, samples: &[f32], token: &CancellationToken) -> Result<String> {
        match self.call(Request::Decode, samples, INFER_TIMEOUT, token)? {
            (Reply::Text(text), _) => Ok(text),
            _ => bail!("invalid Sherpa CUDA ASR response"),
        }
    }

    pub fn generate(
        &self,
        text: &str,
        config: &sherpa_onnx::GenerationConfig,
        token: &CancellationToken,
    ) -> Result<Vec<f32>> {
        let options = TtsRequest {
            speed: config.speed,
            sid: config.sid,
            silence_scale: config.silence_scale,
            reference_sample_rate: config.reference_sample_rate,
            reference_text: config.reference_text.clone(),
            num_steps: config.num_steps,
        };
        let request = Request::Generate {
            text: text.to_owned(),
            options,
        };
        match self.call(
            request,
            config.reference_audio.as_deref().unwrap_or_default(),
            INFER_TIMEOUT,
            token,
        )? {
            (Reply::Audio, audio)
                if !audio.is_empty() && audio.iter().all(|sample| sample.is_finite()) =>
            {
                Ok(audio)
            }
            _ => bail!("invalid Sherpa CUDA TTS response"),
        }
    }
}

impl Worker {
    fn stop(&mut self) {
        self.requests.take();
        // A pipe thread can be blocked sending to the full reply channel.
        self.replies.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopping_worker_unblocks_a_full_reply_channel() {
        let child = Command::new(std::env::current_exe().unwrap())
            .arg("--list")
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let (replies, receive) = mpsc::sync_channel(1);
        replies.send(Ok((Reply::Audio, Vec::new()))).unwrap();
        let reader = thread::spawn(move || {
            assert!(replies.send(Ok((Reply::Audio, Vec::new()))).is_err());
        });
        let mut worker = Worker {
            child,
            requests: None,
            replies: Some(receive),
            threads: vec![reader],
            sample_rate: 0,
            speakers: 0,
        };
        let (done, finished) = mpsc::channel();
        thread::spawn(move || {
            worker.stop();
            assert!(worker.child.try_wait().unwrap().is_some());
            assert!(worker.threads.is_empty());
            done.send(()).unwrap();
        });
        finished.recv_timeout(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn transfers_samples_without_quantizing() {
        let pcm = (0..2050)
            .map(|i| (i as f32 * 0.01).sin())
            .collect::<Vec<_>>();
        let mut wire = Vec::new();
        write_frame(&mut wire, &Request::Decode, &pcm).unwrap();
        let (request, result) = read_frame::<Request>(&mut wire.as_slice()).unwrap();
        assert!(matches!(request, Request::Decode));
        assert_eq!(result, pcm);
    }
}
