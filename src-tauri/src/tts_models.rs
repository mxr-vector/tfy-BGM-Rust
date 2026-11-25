use std::{
    fs,
    io::{Read, Write},
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;

use crate::tts::check_cancelled;

pub const DIR: &str = "tts";
pub const RELEASE_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/tag/tts-models";
const ASSETS: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models";
const AUXILIARY: &[(&str, &str, &str, u64)] = &[
    (
        "vocos_24khz.onnx",
        "https://github.com/k2-fsa/sherpa-onnx/releases/download/vocoder-models/vocos_24khz.onnx",
        "bcb3b970e384161c4d634f0bb9e999ff1c471b34c9bc0b1049a5014065ed3cc0",
        54_157_409,
    ),
    (
        "lexicon-en-us.txt",
        "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/lexicon-en-us.txt",
        "8b67ae549a465eee78f92ecb74644ad90c3c58ce65f19c68baf85878a10ef73a",
        6_305_773,
    ),
];

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TtsModel {
    pub id: &'static str,
    pub name: &'static str,
    pub family: &'static str,
    pub package: &'static str,
    pub size_bytes: u64,
    pub installed: bool,
    pub has_files: bool,
    pub loaded: bool,
    pub path: String,
}

impl TtsModel {
    pub fn acoustic_file(&self) -> &'static str {
        let dir = Path::new(&self.path);
        if self.id.ends_with("int8") && present(dir, "model.int8.onnx") {
            "model.int8.onnx"
        } else {
            "model.onnx"
        }
    }

    pub fn zipvoice_files(&self) -> (&'static str, &'static str) {
        if self.id.ends_with("int8") {
            ("encoder.int8.onnx", "decoder.int8.onnx")
        } else {
            ("encoder.onnx", "decoder.onnx")
        }
    }

    fn complete(&self) -> bool {
        let dir = Path::new(&self.path);
        let required = if self.family == "Kokoro" {
            [
                self.acoustic_file(),
                "voices.bin",
                "tokens.txt",
                "lexicon-us-en.txt",
                "lexicon-zh.txt",
            ]
            .as_slice()
            .iter()
            .all(|name| present(dir, name))
        } else {
            let (encoder, decoder) = self.zipvoice_files();
            [encoder, decoder, "tokens.txt", "lexicon.txt"]
                .iter()
                .all(|name| present(dir, name))
        };
        required
    }
}

struct Spec {
    id: &'static str,
    name: &'static str,
    family: &'static str,
    package: &'static str,
    hash: &'static str,
    size: u64,
}
const SPECS: &[Spec] = &[
    Spec {
        id: "kokoro-int8",
        name: "Kokoro · INT8",
        family: "Kokoro",
        package: "kokoro-int8-multi-lang-v1_1.tar.bz2",
        hash: "259c9c8bc906d6078e4e0570421885dabb68b539c2f970fcbb890a58eeaf3e9c",
        size: 147_052_523,
    },
    Spec {
        id: "kokoro-fp32",
        name: "Kokoro · FP32",
        family: "Kokoro",
        package: "kokoro-multi-lang-v1_1.tar.bz2",
        hash: "b78e1ac42b852e839cceaafbe88f66806bf5860992d1d7b9a3f2ad1198427aed",
        size: 364_855_331,
    },
    Spec {
        id: "zipvoice-int8",
        name: "ZipVoice · INT8",
        family: "ZipVoice",
        package: "sherpa-onnx-zipvoice-distill-int8-zh-en-emilia.tar.bz2",
        hash: "77219c8b40f4ee8d73a7f902305ff6c1128ef9b54461c41b4ca6ed890b6c2803",
        size: 109_162_785,
    },
    Spec {
        id: "zipvoice-fp32",
        name: "ZipVoice · FP32",
        family: "ZipVoice",
        package: "sherpa-onnx-zipvoice-distill-fp32-zh-en-emilia.tar.bz2",
        hash: "3b6729d03bf4ba64deeec113048bdbbe55dbc30580b609be76342e0099fd23a8",
        size: 477_800_463,
    },
];

fn present(dir: &Path, name: &str) -> bool {
    fs::metadata(dir.join(name)).is_ok_and(|info| info.is_file() && info.len() > 0)
}
fn entry(spec: &Spec, dir: &Path) -> TtsModel {
    let mut model = TtsModel {
        id: spec.id,
        name: spec.name,
        family: spec.family,
        package: spec.package,
        size_bytes: spec.size,
        installed: false,
        has_files: dir.is_dir(),
        loaded: false,
        path: dir.to_string_lossy().into_owned(),
    };
    model.installed = model.complete()
        && (spec.family != "ZipVoice"
            || dir.parent().is_some_and(|root| {
                AUXILIARY.iter().all(|(name, _, expected_hash, size)| {
                    fs::metadata(root.join(name))
                        .is_ok_and(|metadata| metadata.is_file() && metadata.len() == *size)
                        && fs::read(root.join(name)).is_ok_and(|data| {
                            format!("{:x}", Sha256::digest(data)) == *expected_hash
                        })
                })
            }));
    model
}

fn package_dir(spec: &Spec) -> &str {
    spec.package.trim_end_matches(".tar.bz2")
}

pub fn model(root: &Path, id: &str) -> Result<TtsModel> {
    let spec = SPECS
        .iter()
        .find(|spec| spec.id == id)
        .context("未知的 TTS 模型")?;
    Ok(entry(spec, &root.join(DIR).join(package_dir(spec))))
}

pub fn catalog(root: &Path) -> Vec<TtsModel> {
    SPECS
        .iter()
        .map(|spec| entry(spec, &root.join(DIR).join(package_dir(spec))))
        .collect()
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadProgress<'a> {
    job_id: uuid::Uuid,
    id: &'a str,
    file: &'a str,
    received: u64,
    total: u64,
}

pub fn install(
    app: &AppHandle,
    models_root: &Path,
    id: &str,
    source: &str,
    job_id: uuid::Uuid,
    token: &CancellationToken,
) -> Result<()> {
    check_cancelled(token)?;
    let spec = SPECS
        .iter()
        .find(|spec| spec.id == id)
        .context("未知的 TTS 模型")?;
    let root = models_root.join(DIR);
    fs::create_dir_all(&root)?;
    let target = root.join(package_dir(spec));
    let existing = entry(spec, &target);
    if existing.installed {
        return Ok(());
    }
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(1800))
        .build()?;
    if spec.family == "ZipVoice" {
        for &(file, url, hash, size) in AUXILIARY {
            let valid_size = fs::metadata(root.join(file))
                .is_ok_and(|metadata| metadata.is_file() && metadata.len() == size);
            if valid_size && format!("{:x}", Sha256::digest(fs::read(root.join(file))?)) == hash {
                continue;
            }
            let target = root.join(file);
            let mirror;
            let url = if source.is_empty() {
                url
            } else {
                mirror = format!("{source}/{file}");
                &mirror
            };
            let tmp = download(
                app, &client, &root, id, job_id, token, file, url, hash, size,
            )?;
            check_cancelled(token)?;
            if target.exists() {
                fs::remove_file(&target)
                    .with_context(|| format!("无法替换校验失败的依赖文件：{file}"))?;
            }
            tmp.persist(target)?;
        }
    }
    check_cancelled(token)?;
    if existing.complete() {
        return Ok(());
    }
    let archive = download(
        app,
        &client,
        &root,
        id,
        job_id,
        token,
        spec.package,
        &format!(
            "{}/{}",
            if source.is_empty() { ASSETS } else { source },
            spec.package
        ),
        spec.hash,
        spec.size,
    )?;
    let staging = tempfile::tempdir_in(&root)?;
    let mut tar = tar::Archive::new(CancellableReader {
        input: bzip2::read::BzDecoder::new(archive.reopen()?),
        token,
    });
    let mut member_count = 0_usize;
    let mut total_size = 0_u64;
    for member in tar.entries()? {
        let mut member = member?;
        let entry_type = member.header().entry_type();
        if !(entry_type.is_file() || entry_type.is_dir()) {
            bail!("TTS 模型压缩包包含链接或特殊文件");
        }
        member_count += 1;
        total_size = total_size
            .checked_add(member.size())
            .context("模型解包文件过大")?;
        if member_count > 4096 || total_size > spec.size.saturating_mul(12) {
            bail!("TTS 模型解包大小超过安全限制");
        }
        let relative = member.path()?.into_owned();
        if !relative.components().all(|component| {
            matches!(
                component,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        }) {
            bail!("TTS 模型压缩包路径无效");
        }
        let target_entry = staging.path().join(&relative);
        if !target_entry.starts_with(staging.path()) {
            bail!("TTS 模型压缩包路径越界");
        }
        member
            .unpack_in(staging.path())
            .context("解压 TTS 模型失败")?;
    }
    let staged = fs::read_dir(staging.path())?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .chain(std::iter::once(staging.path().to_owned()))
        .find(|dir| dir.is_dir() && entry(spec, dir).complete())
        .context("模型压缩包缺少推理文件")?;
    check_cancelled(token)?;
    if target.exists() {
        let backup_dir = tempfile::Builder::new()
            .prefix(".incomplete-")
            .tempdir_in(&root)?
            .keep();
        let backup = backup_dir.join(package_dir(spec));
        fs::rename(&target, &backup).context("无法移开原有的不完整模型目录")?;
        if let Err(err) = fs::rename(staged, &target) {
            fs::rename(&backup, &target)
                .with_context(|| format!("未能恢复原目录，文件保存在 {}", backup.display()))?;
            let _ = fs::remove_dir_all(&backup_dir);
            return Err(err).context("安装 TTS 模型失败");
        }
        fs::remove_dir_all(&backup_dir).context("模型已安装，但清理旧模型副本失败")?;
    } else {
        fs::rename(staged, &target).context("安装 TTS 模型失败")?;
    }
    Ok(())
}

struct CancellableReader<'a, R> {
    input: R,
    token: &'a CancellationToken,
}

impl<R: Read> Read for CancellableReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        check_cancelled(self.token).map_err(std::io::Error::other)?;
        self.input.read(buffer)
    }
}

#[allow(clippy::too_many_arguments)]
fn download(
    app: &AppHandle,
    client: &reqwest::Client,
    root: &Path,
    id: &str,
    job_id: uuid::Uuid,
    token: &CancellationToken,
    file: &str,
    url: &str,
    hash: &str,
    total: u64,
) -> Result<tempfile::NamedTempFile> {
    let mut response =
        tauri::async_runtime::block_on(token.run_until_cancelled(client.get(url).send()))
            .context("语音任务已取消")?
            .and_then(|res| res.error_for_status())
            .with_context(|| format!("无法下载 {file}"))?;
    let mut output = tempfile::NamedTempFile::new_in(root)?;
    let mut hasher = Sha256::new();
    let mut received = 0;
    let mut last = Instant::now();
    let _ = app.emit(
        "tts-download-progress",
        DownloadProgress {
            job_id,
            id,
            file,
            received,
            total,
        },
    );
    loop {
        let Some(chunk) =
            tauri::async_runtime::block_on(token.run_until_cancelled(response.chunk()))
                .context("语音任务已取消")?
                .with_context(|| format!("下载 {file} 中断"))?
        else {
            break;
        };
        check_cancelled(token)?;
        received += chunk.len() as u64;
        if received > total {
            bail!("{file} 超过发布文件大小");
        }
        hasher.update(&chunk);
        output.write_all(&chunk)?;
        if last.elapsed() >= Duration::from_millis(200) || received == total {
            let _ = app.emit(
                "tts-download-progress",
                DownloadProgress {
                    job_id,
                    id,
                    file,
                    received,
                    total,
                },
            );
            last = Instant::now();
        }
    }
    if received != total || format!("{:x}", hasher.finalize()) != hash {
        bail!("{file} 的 SHA-256 校验失败");
    }
    Ok(output)
}
