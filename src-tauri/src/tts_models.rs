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
use bgm_core::prefs::DEFAULT_TTS_DOWNLOAD_URL;

pub const DIR: &str = "tts";
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
    pub recommended: bool,
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
        if self.family == "CosyVoice3" {
            return cosyvoice_complete(dir);
        }
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
const COSYVOICE_ID: &str = "cosyvoice3-0.5b-2512";
const COSYVOICE_ONNX_REPO: &str = "Lourdle/Fun-CosyVoice3-0.5B-2512_ONNX";
const COSYVOICE_ONNX_REV: &str = "879e1017e3143bb19a739309cf3522da2e2a7a6a";
const COSYVOICE_OFFICIAL_REPO: &str = "FunAudioLLM/Fun-CosyVoice3-0.5B-2512";
const COSYVOICE_OFFICIAL_REV: &str = "9f9c56f2514700ef79d64fd0afb693e0d672373b";

struct CosyVoiceFile {
    path: &'static str,
    size: u64,
    hash: &'static str,
    official: bool,
}

const fn onnx(path: &'static str, size: u64, hash: &'static str) -> CosyVoiceFile {
    CosyVoiceFile {
        path,
        size,
        hash,
        official: false,
    }
}

const fn official(path: &'static str, size: u64, hash: &'static str) -> CosyVoiceFile {
    CosyVoiceFile {
        path,
        size,
        hash,
        official: true,
    }
}

const COSYVOICE_FILES: &[CosyVoiceFile] = &[
    onnx(
        "flow_fp32.onnx",
        1_335_578_680,
        "cbcd99556e71c91229fc19ed7655133222a50c35dccbf3b74b0527f18ca024dd",
    ),
    onnx(
        "hift.onnx",
        342_661_124,
        "9831daaeda8662730c703cf3c763617e6a60bd04c23c4f07f595fd6a7601a6f1",
    ),
    official(
        "cosyvoice3.yaml",
        6_934,
        "f5a6b2c6f05139d0f18861a1fe506f751e787026b77c05f7e8fef9f8a4405965",
    ),
    official(
        "llm.pt",
        2_024_669_519,
        "69f43bd545131c30e98947fb360ea8b4dc9916d8e83dded7757c7ea4f5a24970",
    ),
    official(
        "flow.pt",
        1_329_116_148,
        "a6fab32a7825e5b0bc855ddd948f8db9370b0a786fbc249caa4595e95b608e4b",
    ),
    official(
        "hift.pt",
        83_202_622,
        "b279d7641eb97ae55b3b540cfba4f953c26492a2df758328a89a4d007ab87a65",
    ),
    official(
        "campplus.onnx",
        28_303_423,
        "a6ac6a63997761ae2997373e2ee1c47040854b4b759ea41ec48e4e42df0f4d73",
    ),
    official(
        "speech_tokenizer_v3.onnx",
        969_451_503,
        "23236a74175dbdda47afc66dbadd5bcb41303c467a57c261cb8539ad9db9208d",
    ),
    official(
        "CosyVoice-BlankEN/config.json",
        659,
        "168aa1bd401abc3bc262ba15ba4e499627a8b4e006e9d050b47c22de20660185",
    ),
    official(
        "CosyVoice-BlankEN/generation_config.json",
        242,
        "e558847a8b4402616f1273797b015104dc266fe4b520056fca88823ba8f8ebe6",
    ),
    official(
        "CosyVoice-BlankEN/model.safetensors",
        988_097_824,
        "130282af0dfa9fe5840737cc49a0d339d06075f83c5a315c3372c9a0740d0b96",
    ),
    official(
        "CosyVoice-BlankEN/tokenizer_config.json",
        1_287,
        "482bd979881423375ca5414e4e0d94cd7c5349dbb17fffd46b4d36d71e62a1bc",
    ),
    official(
        "CosyVoice-BlankEN/vocab.json",
        2_776_833,
        "ca10d7e9fb3ed18575dd1e277a2579c16d108e32f27439684afa0e10b1440910",
    ),
    official(
        "CosyVoice-BlankEN/merges.txt",
        1_402_109,
        "ac8ff86a72bee70828fbc1119bc4398c6f3a9a6e490d7b0dbe917be025478bd0",
    ),
];

const COSYVOICE_SIZE: u64 = {
    let mut total = 0;
    let mut i = 0;
    while i < COSYVOICE_FILES.len() {
        total += COSYVOICE_FILES[i].size;
        i += 1;
    }
    total
};

const SPECS: &[Spec] = &[
    Spec {
        id: COSYVOICE_ID,
        name: "FunAudioLLM/Fun-CosyVoice3-0.5B-2512",
        family: "CosyVoice3",
        package: "cosyvoice3",
        hash: "",
        size: COSYVOICE_SIZE,
    },
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

fn cosyvoice_complete(dir: &Path) -> bool {
    COSYVOICE_FILES.iter().all(|file| {
        fs::metadata(dir.join(file.path))
            .is_ok_and(|info| info.is_file() && info.len() == file.size)
    })
}

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
        recommended: spec.id == COSYVOICE_ID,
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
        .timeout(Duration::from_secs(7200))
        .user_agent(concat!("tfy-bgm/", env!("CARGO_PKG_VERSION")))
        .referer(false)
        .build()?;
    let source = DownloadSource::parse(source)?;
    if spec.id == COSYVOICE_ID {
        return install_cosyvoice3(app, &client, &target, &source, job_id, token);
    }
    let source = source.legacy_mirror();
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
    publish_model(&staged, &target)
}

fn publish_model(staged: &Path, target: &Path) -> Result<()> {
    match target.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return fs::rename(staged, target).context("安装 TTS 模型失败");
        }
        Ok(info)
            if info.is_dir()
                && target.canonicalize()?
                    == target
                        .parent()
                        .context("模型目录无效")?
                        .canonicalize()?
                        .join(target.file_name().context("模型目录无效")?) => {}
        Ok(_) => bail!("现有模型路径不是普通目录，无法替换"),
        Err(error) => return Err(error).context("无法检查现有模型目录"),
    }
    let backup_dir = tempfile::Builder::new()
        .prefix(".incomplete-")
        .tempdir_in(target.parent().context("模型目录无效")?)?;
    let backup = backup_dir.path().join("model");
    fs::rename(target, &backup).context("无法移开原有的不完整模型目录")?;
    if let Err(error) = fs::rename(staged, target) {
        if let Err(restore) = fs::rename(&backup, target) {
            let kept = backup_dir.keep();
            return Err(restore).with_context(|| {
                format!("安装失败且无法恢复原模型，原文件保存在 {}", kept.display())
            });
        }
        return Err(error).context("安装 TTS 模型失败");
    }
    Ok(())
}

enum DownloadSource<'a> {
    Default,
    ModelScope { origin: String, repo: String },
    Mirror(&'a str),
}

impl<'a> DownloadSource<'a> {
    fn parse(source: &'a str) -> Result<Self> {
        if source.is_empty() {
            return Ok(Self::Default);
        }
        let url = reqwest::Url::parse(source).context("无效的 TTS 下载源")?;
        if !matches!(url.host_str(), Some("modelscope.cn" | "www.modelscope.cn"))
            || !matches!(url.path(), "/models") && !url.path().starts_with("/models/")
        {
            return Ok(Self::Mirror(source));
        }
        let parts: Vec<_> = url.path().trim_end_matches('/').split('/').collect();
        let (owner, name) = match parts.as_slice() {
            ["", "models", owner, name] | ["", "models", owner, name, "files"] => (*owner, *name),
            _ => bail!("请输入 ModelScope 模型仓库首页或 /files 地址"),
        };
        if ![owner, name].iter().all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        }) {
            bail!("ModelScope 模型仓库名称无效");
        }
        if source == DEFAULT_TTS_DOWNLOAD_URL {
            return Ok(Self::Default);
        }
        Ok(Self::ModelScope {
            origin: url.origin().ascii_serialization(),
            repo: format!("{owner}/{name}"),
        })
    }

    fn legacy_mirror(&self) -> &str {
        match self {
            Self::Mirror(source) => source,
            _ => "",
        }
    }

    fn cosyvoice_url(&self, file: &CosyVoiceFile) -> Result<reqwest::Url> {
        let default_origin = "https://www.modelscope.cn";
        let (origin, repo, revision) = if file.official {
            (
                default_origin,
                COSYVOICE_OFFICIAL_REPO,
                COSYVOICE_OFFICIAL_REV,
            )
        } else if let Self::ModelScope { origin, repo } = self {
            (
                origin.as_str(),
                repo.as_str(),
                if repo == COSYVOICE_ONNX_REPO {
                    COSYVOICE_ONNX_REV
                } else {
                    "master"
                },
            )
        } else {
            (default_origin, COSYVOICE_ONNX_REPO, COSYVOICE_ONNX_REV)
        };
        let mut url = reqwest::Url::parse(&format!("{origin}/api/v1/models/{repo}/repo"))?;
        url.query_pairs_mut()
            .append_pair("Revision", revision)
            .append_pair("FilePath", file.path);
        Ok(url)
    }
}

pub fn validate_download_source(source: &str) -> Result<()> {
    DownloadSource::parse(source).map(|_| ())
}

fn install_cosyvoice3(
    app: &AppHandle,
    client: &reqwest::Client,
    target: &Path,
    source: &DownloadSource<'_>,
    job_id: uuid::Uuid,
    token: &CancellationToken,
) -> Result<()> {
    let root = target.parent().context("模型目录无效")?;
    let staging = tempfile::tempdir_in(root)?;
    let mut received = 0;
    for file in COSYVOICE_FILES {
        check_cancelled(token)?;
        let url = source.cosyvoice_url(file)?;
        let destination = staging.path().join(file.path);
        fs::create_dir_all(destination.parent().context("模型文件路径无效")?)?;
        let temporary = download_verified(
            client,
            staging.path(),
            token,
            file.path,
            url.as_str(),
            file.hash,
            file.size,
            |bytes| {
                let _ = app.emit(
                    "tts-download-progress",
                    DownloadProgress {
                        job_id,
                        id: COSYVOICE_ID,
                        file: file.path,
                        received: received + bytes,
                        total: COSYVOICE_SIZE,
                    },
                );
            },
        )?;
        temporary.persist(destination)?;
        received += file.size;
    }
    if !cosyvoice_complete(staging.path()) {
        bail!("CosyVoice3 模型文件不完整");
    }
    let config = target.join("runtime.json");
    if config.symlink_metadata().is_ok_and(|info| info.is_file()) {
        fs::copy(config, staging.path().join("runtime.json"))?;
    }
    check_cancelled(token)?;
    publish_model(staging.path(), target)
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
    download_verified(client, root, token, file, url, hash, total, |received| {
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
    })
}

#[allow(clippy::too_many_arguments)]
fn download_verified(
    client: &reqwest::Client,
    root: &Path,
    token: &CancellationToken,
    file: &str,
    url: &str,
    hash: &str,
    total: u64,
    mut progress: impl FnMut(u64),
) -> Result<tempfile::NamedTempFile> {
    progress(0);
    let mut response =
        tauri::async_runtime::block_on(token.run_until_cancelled(client.get(url).send()))
            .context("语音任务已取消")?
            .and_then(|res| res.error_for_status())
            .with_context(|| format!("无法下载 {file}"))?;
    let mut output = tempfile::NamedTempFile::new_in(root)?;
    let mut hasher = Sha256::new();
    let mut received = 0;
    let mut last = Instant::now();
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
            progress(received);
            last = Instant::now();
        }
    }
    if received != total {
        bail!("{file} 大小与发布文件不符");
    }
    if format!("{:x}", hasher.finalize()) != hash {
        bail!("{file} 的 SHA-256 校验失败");
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_route_modelscope_to_cosyvoice_and_flat_mirrors_to_sherpa() {
        let file = &COSYVOICE_FILES[0];
        for source in [
            DEFAULT_TTS_DOWNLOAD_URL,
            "https://modelscope.cn/models/Lourdle/Fun-CosyVoice3-0.5B-2512_ONNX",
        ] {
            let source = DownloadSource::parse(source).unwrap();
            assert_eq!(source.legacy_mirror(), "");
            let url = source.cosyvoice_url(file).unwrap();
            assert!(
                url.path()
                    .ends_with("/Lourdle/Fun-CosyVoice3-0.5B-2512_ONNX/repo")
            );
            assert!(
                url.query_pairs()
                    .any(|(key, value)| key == "Revision" && value == COSYVOICE_ONNX_REV)
            );
        }
        let source = DownloadSource::parse("https://example.com/models").unwrap();
        assert_eq!(source.legacy_mirror(), "https://example.com/models");
        assert_eq!(
            source.cosyvoice_url(file).unwrap().host_str(),
            Some("www.modelscope.cn")
        );
        let custom =
            DownloadSource::parse("https://www.modelscope.cn/models/example/mirror/files").unwrap();
        assert!(
            custom
                .cosyvoice_url(file)
                .unwrap()
                .path()
                .ends_with("/example/mirror/repo")
        );
        assert!(
            custom
                .cosyvoice_url(&COSYVOICE_FILES[2])
                .unwrap()
                .path()
                .contains(COSYVOICE_OFFICIAL_REPO)
        );
        assert!(DownloadSource::parse("https://modelscope.cn/models/owner").is_err());
    }

    #[test]
    fn failed_publish_restores_partial_install() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("cosyvoice3");
        let staged = root.path().join("staged");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("runtime.json"), b"{}").unwrap();
        assert!(publish_model(&staged, &target).is_err());
        assert_eq!(fs::read(target.join("runtime.json")).unwrap(), b"{}");
    }

    #[test]
    fn download_checks_size_and_digest_before_installing() {
        use std::net::TcpListener;
        let dir = tempfile::tempdir().unwrap();
        let client = reqwest::Client::new();
        let hash = format!("{:x}", Sha256::digest(b"abc"));
        for (expected_hash, size, succeeds) in [
            (hash.as_str(), 3, true),
            ("wrong", 3, false),
            (hash.as_str(), 2, false),
        ] {
            let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                let mut request = [0; 4096];
                let _ = socket.read(&mut request).unwrap();
                socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc",
                    )
                    .unwrap();
            });
            let mut bytes = Vec::new();
            let result = download_verified(
                &client,
                dir.path(),
                &CancellationToken::new(),
                "model",
                &url,
                expected_hash,
                size,
                |count| bytes.push(count),
            );
            server.join().unwrap();
            assert_eq!(result.is_ok(), succeeds);
            if let Ok(file) = result {
                assert_eq!(fs::read(file.path()).unwrap(), b"abc");
                assert_eq!(bytes, [0, 3]);
            }
        }
        let token = CancellationToken::new();
        token.cancel();
        let error = download_verified(
            &client,
            dir.path(),
            &token,
            "model",
            "http://127.0.0.1:1",
            &hash,
            3,
            |_| {},
        )
        .unwrap_err();
        assert!(error.to_string().contains("取消"));
    }
}
