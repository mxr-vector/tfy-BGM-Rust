use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, ensure};
use serde::Serialize;
use tempfile::{Builder, TempDir};

use super::{sherpa, subtitles};
use subtitles::generation::PendingSubtitle;

mod models;
mod worker;
pub use models::{ACCOMPANIMENT, Catalog, Model, Target};
pub use worker::run as run_worker;

pub const WORKER_FLAG: &str = "--separation-worker";
const VOCALS_WAV: &str = "vocals.wav";
const ACCOMPANIMENT_WAV: &str = "accompaniment.wav";

pub struct Request {
    pub subtitles: PathBuf,
    pub key: String,
    pub source: PathBuf,
    pub model: Model,
    pub target: Target,
    pub accompaniment: PathBuf,
}

impl Request {
    pub fn generate(self) -> Result<Prepared> {
        let fingerprint = Fingerprint::read(&self.source)?;
        let executable = std::env::current_exe()?;
        let runtime = sherpa::runtime_library(&executable).map_err(anyhow::Error::msg)?;
        let base = self.subtitles.with_file_name("separations");
        fs::create_dir_all(&base).context("无法创建分离产物目录")?;
        ensure!(
            fs::symlink_metadata(&base)?.is_dir(),
            "分离产物根目录必须是普通目录"
        );
        let scratch = Builder::new()
            .prefix(".separation-")
            .suffix(".part")
            .tempdir_in(base)?;
        let staging = scratch.path().join("stems");
        fs::create_dir(&staging)?;
        let id = self.model.id.clone();
        let name = self.model.name.clone();
        let vocals = self.model.resolve(scratch.path())?;
        let engine = vocals.label();
        let job = worker::Job {
            source: fingerprint.path.clone(),
            runtime,
            vocals,
            target: self.target,
            accompaniment: models::Engine::Uvr(self.accompaniment),
        };
        let job_path = staging.join(worker::JOB_FILE);
        serde_json::to_writer(File::create(&job_path)?, &job)?;
        let log = scratch.path().join("worker.log");
        let status = Command::new(executable)
            .arg(WORKER_FLAG)
            .arg(&job_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(File::create(&log)?)
            .status()
            .context("无法启动人声分离子进程")?;
        let result_path = staging.join(worker::RESULT_FILE);
        let result = File::open(&result_path)
            .ok()
            .and_then(|file| serde_json::from_reader::<_, Result<(), String>>(file).ok());
        if let Some(Err(error)) = result {
            return Err(anyhow!(error));
        }
        ensure!(
            status.success() && result.is_some(),
            "人声分离子进程异常退出（{status}）{}",
            log_tail(&log)
        );
        fs::remove_file(&job_path)?;
        fs::remove_file(&result_path)?;
        let sample_rate = worker::verify_wave(&staging.join(VOCALS_WAV))?;
        worker::verify_wave(&staging.join(ACCOMPANIMENT_WAV))?;
        Ok(Prepared {
            subtitles: self.subtitles,
            key: self.key,
            fingerprint,
            _scratch: scratch,
            staging,
            model_id: id,
            model_name: name,
            engine,
            sample_rate,
            target: self.target,
        })
    }
}

pub struct Prepared {
    subtitles: PathBuf,
    key: String,
    fingerprint: Fingerprint,
    _scratch: TempDir,
    staging: PathBuf,
    model_id: String,
    model_name: String,
    engine: &'static str,
    sample_rate: u32,
    target: Target,
}

impl Prepared {
    pub fn staged_vocals(&self) -> PathBuf {
        self.staging.join(VOCALS_WAV)
    }

    pub fn publish(self, current_source: &Path) -> Result<PathBuf> {
        self.publish_output(current_source, None)
    }

    pub(super) fn publish_subtitle(
        self,
        song: &bgm_core::Song,
        subtitle: PendingSubtitle,
    ) -> Result<PathBuf> {
        self.publish_output(Path::new(&song.path), Some((song, subtitle)))
    }

    fn publish_output(
        self,
        current_source: &Path,
        subtitle: Option<(&bgm_core::Song, PendingSubtitle)>,
    ) -> Result<PathBuf> {
        fs::create_dir_all(&self.subtitles).context("无法创建字幕目录")?;
        let _lock = subtitles::lock_record(&self.subtitles, subtitles::key_hash(&self.key))
            .map_err(anyhow::Error::msg)?;
        ensure!(
            Fingerprint::read(current_source)? == self.fingerprint,
            "源音频已经更改，请重新分离"
        );
        let previous = subtitles::verified_separation_directory(&self.subtitles, &self.key)
            .map_err(anyhow::Error::msg)?;
        let dest = if let Some(previous) = &previous {
            for name in ["subtitles.srt", "subtitles.lrc"]
                .into_iter()
                .filter(|_| subtitle.is_none())
            {
                let file = previous.join(name);
                match fs::symlink_metadata(&file) {
                    Ok(metadata) if metadata.is_file() => {
                        fs::hard_link(&file, self.staging.join(name))
                            .with_context(|| format!("无法保留已有字幕 {name}"))?;
                    }
                    Ok(_) => return Err(anyhow!("已有字幕 {name} 不是普通文件")),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
            previous.clone()
        } else {
            self.subtitles
                .with_file_name("separations")
                .canonicalize()?
                .join(subtitles::separation_stem(&self.key))
        };
        let subtitle = subtitle
            .map(|(song, pending)| {
                pending.stage(
                    &self.subtitles,
                    &self.staging,
                    &dest,
                    song,
                    &self.model_name,
                    models::ACCOMPANIMENT,
                )
            })
            .transpose()?;
        let record = Record {
            key: &self.key,
            model_id: &self.model_id,
            model_name: &self.model_name,
            engine: self.engine,
            target: self.target,
            accompaniment_model_name: models::ACCOMPANIMENT,
            vocals_path: dest.join(VOCALS_WAV),
            accompaniment_path: dest.join(ACCOMPANIMENT_WAV),
            accompaniment_kind: "伴奏",
            sample_rate: self.sample_rate,
            source_path: current_source,
            created_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        };
        let manifest = File::create(self.staging.join("separation.json"))?;
        serde_json::to_writer_pretty(&manifest, &record)?;
        manifest.sync_all()?;
        if previous.is_some() {
            replace_directory(&self.staging, &dest)?;
        } else {
            fs::rename(&self.staging, &dest).context("无法发布分离产物")?;
        }
        if let Some(subtitle) = subtitle
            && let Err(error) = subtitle.publish()
        {
            let rollback = if previous.is_some() {
                replace_directory(&self.staging, &dest)
            } else {
                fs::rename(&dest, &self.staging).map_err(anyhow::Error::from)
            };
            if let Err(rollback) = rollback {
                let retained = self._scratch.keep();
                return Err(anyhow!(
                    "{error:#}，无法恢复分离产物：{rollback:#}。暂存目录：{}",
                    retained.display()
                ));
            }
            return Err(error);
        }
        Ok(dest)
    }
}

#[derive(PartialEq)]
struct Fingerprint {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
    #[cfg(unix)]
    identity: (u64, u64),
}

impl Fingerprint {
    fn read(path: &Path) -> Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        let path = path.canonicalize().context("无法访问源音频")?;
        let metadata = fs::metadata(&path).context("无法读取源音频状态")?;
        ensure!(metadata.is_file(), "源音频必须是普通文件");
        Ok(Self {
            path,
            size: metadata.len(),
            modified: metadata.modified()?,
            #[cfg(unix)]
            identity: (metadata.dev(), metadata.ino()),
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Record<'a> {
    key: &'a str,
    model_id: &'a str,
    model_name: &'a str,
    engine: &'a str,
    target: Target,
    accompaniment_model_name: &'a str,
    vocals_path: PathBuf,
    accompaniment_path: PathBuf,
    accompaniment_kind: &'a str,
    sample_rate: u32,
    source_path: &'a Path,
    created_at_ms: u64,
}

fn log_tail(path: &Path) -> String {
    let tail = (|| -> io::Result<_> {
        let mut log = File::open(path)?;
        let bytes = log.metadata()?.len();
        log.seek(SeekFrom::Start(bytes.saturating_sub(768)))?;
        let mut tail = Vec::new();
        log.take(768).read_to_end(&mut tail)?;
        Ok(tail)
    })()
    .unwrap_or_default();
    let text = String::from_utf8_lossy(&tail);
    if text.trim().is_empty() {
        String::new()
    } else {
        format!("：{}", text.trim())
    }
}

#[cfg(target_os = "macos")]
fn replace_directory(staged: &Path, dest: &Path) -> Result<()> {
    use std::ffi::{CString, c_char, c_int, c_uint};
    use std::os::unix::ffi::OsStrExt;

    const RENAME_SWAP: c_uint = 2;
    unsafe extern "C" {
        fn renamex_np(from: *const c_char, to: *const c_char, flags: c_uint) -> c_int;
    }
    let staged = CString::new(staged.as_os_str().as_bytes())?;
    let dest = CString::new(dest.as_os_str().as_bytes())?;
    if unsafe { renamex_np(staged.as_ptr(), dest.as_ptr(), RENAME_SWAP) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error()).context("无法替换分离产物")
    }
}

#[cfg(not(target_os = "macos"))]
fn replace_directory(staged: &Path, dest: &Path) -> Result<()> {
    let backup = dest.with_extension(format!("{:016x}.part", fastrand::u64(..)));
    fs::rename(dest, &backup).context("无法暂存已有分离产物")?;
    if let Err(error) = fs::rename(staged, dest) {
        fs::rename(&backup, dest)
            .with_context(|| format!("替换失败（{error}），原分离产物位于 {}", backup.display()))?;
        return Err(error).context("无法替换分离产物");
    }
    let _ = fs::remove_dir_all(backup);
    Ok(())
}

pub fn audio_paths(directory: &Path, key: &str) -> [Option<PathBuf>; 2] {
    let Ok(Some(dir)) = subtitles::verified_separation_directory(directory, key) else {
        return [None, None];
    };
    [VOCALS_WAV, ACCOMPANIMENT_WAV].map(|name| {
        let path = dir.join(name);
        fs::symlink_metadata(&path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 44)
            .then_some(path)
    })
}

pub(crate) fn result_document(
    directory: &Path,
    key: &str,
) -> Result<Option<serde_json::Value>, String> {
    use serde_json::json;

    let Some((directory, record)) = subtitles::separation_record(directory, key)? else {
        return Ok(None);
    };
    let files = [
        ("vocals", "vocalsPath", VOCALS_WAV),
        ("accompaniment", "accompanimentPath", ACCOMPANIMENT_WAV),
    ].map(|(kind, field, name)| {
        let path = directory.join(name);
        let bytes = fs::symlink_metadata(&path).ok().filter(|meta| meta.is_file()).map(|meta| meta.len());
        let path = (bytes.is_some() || record.get(field).and_then(serde_json::Value::as_str).is_some()).then_some(path);
        json!({ "kind": kind, "path": path, "exists": bytes.is_some(), "bytes": bytes.unwrap_or_default() })
    });
    Ok(Some(json!({ "separation": record, "files": files })))
}

pub fn delete_standalone(directory: &Path, key: &str) -> Result<(), String> {
    subtitles::delete_standalone_separation(directory, key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn regeneration_preserves_existing_generation_until_source_and_manifest_match() {
        let config = tempfile::tempdir().unwrap();
        let subtitles = config.path().join("subtitles");
        let base = config.path().join("separations");
        fs::create_dir(&base).unwrap();
        let source = config.path().join("track.mp3");
        fs::write(&source, b"original audio").unwrap();

        let stage = |model: &str| {
            let scratch = tempfile::tempdir_in(&base).unwrap();
            let staging = scratch.path().join("stems");
            fs::create_dir(&staging).unwrap();
            fs::write(staging.join(VOCALS_WAV), model).unwrap();
            fs::write(staging.join(ACCOMPANIMENT_WAV), model).unwrap();
            Prepared {
                subtitles: subtitles.clone(),
                key: "music/track.mp3".into(),
                fingerprint: Fingerprint::read(&source).unwrap(),
                _scratch: scratch,
                staging,
                model_id: model.into(),
                model_name: model.into(),
                engine: "UVR MDX-Net",
                sample_rate: 44_100,
                target: Target::Vocals,
            }
        };

        let dir = stage("original").publish(&source).unwrap();
        fs::write(dir.join("subtitles.srt"), b"existing subtitles").unwrap();

        let stale = stage("stale");
        let stale_path = stale.staging.clone();
        fs::write(&source, b"modified").unwrap();
        assert!(stale.publish(&source).is_err());
        assert!(!stale_path.exists());
        assert_eq!(fs::read(dir.join(VOCALS_WAV)).unwrap(), b"original");

        let next = stage("new");
        let next_path = next.staging.clone();
        assert_eq!(next.publish(&source).unwrap(), dir);
        assert!(!next_path.exists());
        assert_eq!(fs::read(dir.join(VOCALS_WAV)).unwrap(), b"new");
        assert_eq!(fs::read(dir.join(ACCOMPANIMENT_WAV)).unwrap(), b"new");
        assert_eq!(
            fs::read(dir.join("subtitles.srt")).unwrap(),
            b"existing subtitles"
        );
        let manifest: Value =
            serde_json::from_reader(File::open(dir.join("separation.json")).unwrap()).unwrap();
        assert_eq!(manifest["modelId"], "new");
        assert_eq!(manifest["key"], "music/track.mp3");
        assert_eq!(manifest["accompanimentKind"], "伴奏");
        assert_eq!(manifest["sampleRate"], 44_100);
        let manifest_path = dir.join("separation.json");
        let original_manifest = fs::read(&manifest_path).unwrap();
        fs::write(&manifest_path, br#"{"key":"unrelated"}"#).unwrap();
        assert!(stage("conflict").publish(&source).is_err());
        assert!(delete_standalone(&subtitles, "music/track.mp3").is_err());
        assert_eq!(fs::read(dir.join(VOCALS_WAV)).unwrap(), b"new");

        fs::write(&manifest_path, original_manifest).unwrap();
        delete_standalone(&subtitles, "music/track.mp3").unwrap();
        assert!(!dir.exists());
        assert_eq!(fs::read(&source).unwrap(), b"modified");
    }
}
