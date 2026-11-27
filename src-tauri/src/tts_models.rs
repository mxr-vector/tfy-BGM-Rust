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
use bgm_core::prefs::{
    DEFAULT_TTS_DOWNLOAD_URL, DEFAULT_TTS_MIRROR_URL, DEFAULT_TTS_RELEASE_URL,
    TTS_COSYVOICE_MODEL_ID, TTS_SHERPA_MODEL_IDS,
};

pub const DIR: &str = "tts";
const ASSETS: &str = DEFAULT_TTS_MIRROR_URL;
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
    pub device: Option<&'static str>,
    pub gpu_environment_ready: Option<bool>,
    pub gpu_environment_present: bool,
    pub path: String,
    /// 默认浏览地址；「访问下载源」在未自定义时打开它。
    pub default_download_url: String,
    /// 可安装的默认基址；编辑下载源时作为输入占位。
    pub default_source_url: String,
    /// 用户按模型覆盖的下载源；空字符串表示使用内置默认源。
    pub custom_download_url: String,
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
pub(crate) const COSYVOICE_ID: &str = TTS_COSYVOICE_MODEL_ID;
const KOKORO_INT8_ID: &str = TTS_SHERPA_MODEL_IDS[0];
const KOKORO_FP32_ID: &str = TTS_SHERPA_MODEL_IDS[1];
const ZIPVOICE_INT8_ID: &str = TTS_SHERPA_MODEL_IDS[2];
const ZIPVOICE_FP32_ID: &str = TTS_SHERPA_MODEL_IDS[3];
const COSYVOICE_ONNX_REPO: &str = "zhangluoyang/cosyvoice3-onnx";
const COSYVOICE_ONNX_REV: &str = "4e43c04c7bb86d5551da5bc7dc912f3c326ffe30";
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

/// 只下载非流式一套可跑通的文件：跳过 model_stream.onnx、旧版 llm.pt/flow.pt/hift.pt。
const COSYVOICE_FILES: &[CosyVoiceFile] = &[
    onnx(
        "frontend/campplus.onnx",
        28_303_423,
        "a6ac6a63997761ae2997373e2ee1c47040854b4b759ea41ec48e4e42df0f4d73",
    ),
    onnx(
        "frontend/speech_tokenizer_v3.onnx",
        969_451_503,
        "23236a74175dbdda47afc66dbadd5bcb41303c467a57c261cb8539ad9db9208d",
    ),
    onnx(
        "llm/model.onnx",
        436_205,
        "05e08708b14d28c6eb25f888b967a0850b077dbe2ffa3155d893da82becd8df1",
    ),
    onnx(
        "llm/model.onnx.data",
        1_012_284_672,
        "ce8d1bae8a03d6fd5c9917fe247982ed08da35a18567f51d5695191914d99686",
    ),
    onnx(
        "flow/flow_conditioner.onnx",
        4_464_695,
        "73de655c255b01f754223ef31685cf61800f6454c94e0965ff80a136ea069520",
    ),
    onnx(
        "flow/flow_dit_step.onnx",
        1_045_701,
        "ef559341bcb62d3e97130a591a0a1fc6ebf7a81286b5622457024896c4ce59e4",
    ),
    onnx(
        "flow/flow_dit_step.onnx.data",
        1_324_568_576,
        "c9564d8ff035fb62142442aaaa1787c6dd2007790b4b0ae8c27c5860de16a371",
    ),
    onnx(
        "hift/model.onnx",
        83_397_707,
        "ab1e8e957d6b2460e8521b3575c51df3a69f94f1bca5936309d8e266ec6ee9d1",
    ),
    official(
        "cosyvoice3.yaml",
        6_934,
        "f5a6b2c6f05139d0f18861a1fe506f751e787026b77c05f7e8fef9f8a4405965",
    ),
    official(
        "CosyVoice-BlankEN/config.json",
        659,
        "168aa1bd401abc3bc262ba15ba4e499627a8b4e006e9d050b47c22de20660185",
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
        id: KOKORO_INT8_ID,
        name: "Kokoro · INT8",
        family: "Kokoro",
        package: "kokoro-int8-multi-lang-v1_1.tar.bz2",
        hash: "259c9c8bc906d6078e4e0570421885dabb68b539c2f970fcbb890a58eeaf3e9c",
        size: 147_052_523,
    },
    Spec {
        id: KOKORO_FP32_ID,
        name: "Kokoro · FP32",
        family: "Kokoro",
        package: "kokoro-multi-lang-v1_1.tar.bz2",
        hash: "b78e1ac42b852e839cceaafbe88f66806bf5860992d1d7b9a3f2ad1198427aed",
        size: 364_855_331,
    },
    Spec {
        id: ZIPVOICE_INT8_ID,
        name: "ZipVoice · INT8",
        family: "ZipVoice",
        package: "sherpa-onnx-zipvoice-distill-int8-zh-en-emilia.tar.bz2",
        hash: "77219c8b40f4ee8d73a7f902305ff6c1128ef9b54461c41b4ca6ed890b6c2803",
        size: 109_162_785,
    },
    Spec {
        id: ZIPVOICE_FP32_ID,
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

fn spec(id: &str) -> Result<&'static Spec> {
    SPECS
        .iter()
        .find(|spec| spec.id == id)
        .with_context(|| format!("未知的 TTS 模型：{id}"))
}

/// 默认地址：浏览地址用于「访问下载源」，安装基址用于编辑占位与镜像校验。
fn default_urls(spec: &Spec) -> (&'static str, &'static str) {
    if spec.family == "CosyVoice3" {
        (DEFAULT_TTS_DOWNLOAD_URL, DEFAULT_TTS_DOWNLOAD_URL)
    } else {
        (DEFAULT_TTS_RELEASE_URL, DEFAULT_TTS_MIRROR_URL)
    }
}

pub fn default_urls_for(id: &str) -> Result<(&'static str, &'static str)> {
    Ok(default_urls(spec(id)?))
}

fn entry(spec: &Spec, dir: &Path) -> TtsModel {
    let (default_download_url, default_source_url) = default_urls(spec);
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
        device: None,
        gpu_environment_ready: None,
        gpu_environment_present: false,
        path: dir.to_string_lossy().into_owned(),
        default_download_url: default_download_url.to_owned(),
        default_source_url: default_source_url.to_owned(),
        custom_download_url: String::new(),
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
    let spec = spec(id)?;
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
    let spec = spec(id)?;
    validate_source(id, source)?;
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

/// 模型文件后缀；自定义镜像必须填目录而不是具体文件。
const FILE_SUFFIXES: [&str; 6] = [".tar.bz2", ".onnx", ".data", ".json", ".txt", ".bin"];

fn is_modelscope_repo(url: &reqwest::Url) -> bool {
    matches!(url.host_str(), Some("modelscope.cn" | "www.modelscope.cn"))
        && (url.path() == "/models" || url.path().starts_with("/models/"))
}

/// 按模型校验下载源：CosyVoice3 只接受 ModelScope 仓库地址，
/// Kokoro / ZipVoice 只接受可拼接文件名的 HTTP(S) 镜像目录。
pub fn validate_source(id: &str, source: &str) -> Result<()> {
    let spec = spec(id)?;
    let source = source.trim().trim_end_matches('/');
    if source.is_empty() {
        return Ok(());
    }
    let url = reqwest::Url::parse(source).context("请输入有效的 HTTP 或 HTTPS 地址")?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        bail!("请输入有效的 HTTP 或 HTTPS 地址");
    }
    if spec.family == "CosyVoice3" {
        if !is_modelscope_repo(&url) {
            bail!("CosyVoice3 下载源需为 ModelScope 模型仓库地址（例如 .../files）");
        }
        DownloadSource::parse(source)?;
        return Ok(());
    }
    if is_modelscope_repo(&url) {
        bail!("Kokoro / ZipVoice 下载源需为可直接拼接文件名的 HTTP(S) 镜像目录");
    }
    if url.path().contains("/releases/tag/") {
        bail!("请填写可直接下载的镜像目录，而不是 GitHub release 页面");
    }
    let lower = source.to_ascii_lowercase();
    if FILE_SUFFIXES.iter().any(|suffix| lower.ends_with(suffix)) {
        bail!("请填写镜像目录基地址，而不是具体文件地址");
    }
    DownloadSource::parse(source)?;
    Ok(())
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
        let destination = staging.path().join(file.path);
        fs::create_dir_all(destination.parent().context("模型文件路径无效")?)?;
        let progress = |bytes| {
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
        };
        let temporary = match reuse_cosyvoice_file(target, staging.path(), file, token)? {
            Some(verified) => {
                progress(file.size);
                verified
            }
            None => download_verified(
                client,
                staging.path(),
                token,
                file.path,
                source.cosyvoice_url(file)?.as_str(),
                file.hash,
                file.size,
                progress,
            )?,
        };
        temporary.persist(destination)?;
        received += file.size;
    }
    if !cosyvoice_complete(staging.path()) {
        bail!("CosyVoice3 模型文件不完整");
    }
    check_cancelled(token)?;
    publish_model(staging.path(), target)
}

fn reuse_cosyvoice_file(
    target: &Path,
    staging: &Path,
    file: &CosyVoiceFile,
    token: &CancellationToken,
) -> Result<Option<tempfile::NamedTempFile>> {
    let source = target.join(file.path);
    if !source
        .symlink_metadata()
        .is_ok_and(|info| info.is_file() && info.len() == file.size)
    {
        return Ok(None);
    }
    let Ok(mut source) = fs::File::open(source) else {
        return Ok(None);
    };
    let mut output = tempfile::NamedTempFile::new_in(staging)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 128 * 1024];
    let mut size = 0;
    loop {
        check_cancelled(token)?;
        let count = match source.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Ok(None),
        };
        size += count as u64;
        if size > file.size {
            return Ok(None);
        }
        hasher.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
    }
    if size == file.size && format!("{:x}", hasher.finalize()) == file.hash {
        Ok(Some(output))
    } else {
        Ok(None)
    }
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
        let download: Vec<_> = COSYVOICE_FILES
            .iter()
            .filter(|file| !file.official)
            .collect();
        assert_eq!(download.len(), 8);
        assert!(
            COSYVOICE_FILES
                .iter()
                .all(|file| file.path != "hift/model_stream.onnx")
        );
        let file = download[0];
        assert_eq!(file.path, "frontend/campplus.onnx");
        assert!(
            download
                .iter()
                .any(|file| file.path.ends_with(".onnx.data"))
        );
        assert_eq!(COSYVOICE_SIZE, 3_428_140_304);
        for source in [
            DEFAULT_TTS_DOWNLOAD_URL,
            "https://modelscope.cn/models/zhangluoyang/cosyvoice3-onnx",
        ] {
            let source = DownloadSource::parse(source).unwrap();
            assert_eq!(source.legacy_mirror(), "");
            let url = source.cosyvoice_url(file).unwrap();
            assert!(url.path().ends_with("/zhangluoyang/cosyvoice3-onnx/repo"));
            assert!(
                url.query_pairs()
                    .any(|(key, value)| key == "Revision" && value == COSYVOICE_ONNX_REV)
            );
            assert!(
                url.query_pairs()
                    .any(|(key, value)| key == "FilePath" && value == file.path)
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
        for dependency in COSYVOICE_FILES.iter().filter(|file| file.official) {
            assert!(
                custom
                    .cosyvoice_url(dependency)
                    .unwrap()
                    .path()
                    .contains(COSYVOICE_OFFICIAL_REPO)
            );
        }
        assert!(DownloadSource::parse("https://modelscope.cn/models/owner").is_err());
    }

    #[test]
    fn spec_ids_match_core_constants() {
        assert_eq!(SPECS.len(), 1 + TTS_SHERPA_MODEL_IDS.len());
        assert!(SPECS.iter().any(|spec| spec.id == TTS_COSYVOICE_MODEL_ID));
        for id in TTS_SHERPA_MODEL_IDS {
            assert!(SPECS.iter().any(|spec| spec.id == id), "缺少模型 {id}");
        }
    }

    #[test]
    fn default_urls_follow_model_family() {
        let (browse, base) = default_urls_for(COSYVOICE_ID).unwrap();
        assert_eq!(browse, DEFAULT_TTS_DOWNLOAD_URL);
        assert_eq!(base, DEFAULT_TTS_DOWNLOAD_URL);

        for id in TTS_SHERPA_MODEL_IDS {
            let (browse, base) = default_urls_for(id).unwrap();
            assert_eq!(browse, DEFAULT_TTS_RELEASE_URL);
            assert_eq!(base, DEFAULT_TTS_MIRROR_URL);
        }
        assert!(default_urls_for("unknown").is_err());
    }

    #[test]
    fn source_validation_is_family_aware() {
        assert!(validate_source(COSYVOICE_ID, "").is_ok());
        assert!(validate_source(COSYVOICE_ID, DEFAULT_TTS_DOWNLOAD_URL).is_ok());
        assert!(validate_source(COSYVOICE_ID, "https://mirror.example.com/tts").is_err());
        assert!(
            validate_source(
                COSYVOICE_ID,
                "https://www.modelscope.cn/models/owner/repo/files"
            )
            .is_ok()
        );

        assert!(validate_source(KOKORO_INT8_ID, "").is_ok());
        assert!(validate_source(KOKORO_INT8_ID, "https://mirror.example.com/tts").is_ok());
        assert!(validate_source(KOKORO_INT8_ID, "https://mirror.example.com/tts/").is_ok());
        assert!(validate_source(KOKORO_INT8_ID, DEFAULT_TTS_DOWNLOAD_URL).is_err());
        assert!(
            validate_source(
                KOKORO_INT8_ID,
                "https://mirror.example.com/tts/kokoro.tar.bz2"
            )
            .is_err()
        );
        assert!(
            validate_source(
                KOKORO_INT8_ID,
                "https://github.com/k2-fsa/sherpa-onnx/releases/tag/tts-models"
            )
            .is_err()
        );
        assert!(validate_source("unknown", "https://mirror.example.com/tts").is_err());
    }

    #[test]
    fn reused_cosyvoice_files_must_match_digest() {
        let target = tempfile::tempdir().unwrap();
        let staging = tempfile::tempdir().unwrap();
        let token = CancellationToken::new();
        let file = official(
            "dependency.onnx",
            3,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        );
        let path = target.path().join(file.path);
        fs::write(&path, b"bad").unwrap();
        assert!(
            reuse_cosyvoice_file(target.path(), staging.path(), &file, &token)
                .unwrap()
                .is_none()
        );
        fs::write(&path, b"abc").unwrap();
        let verified = reuse_cosyvoice_file(target.path(), staging.path(), &file, &token)
            .unwrap()
            .unwrap();
        assert_eq!(fs::read(verified.path()).unwrap(), b"abc");
        token.cancel();
        assert!(reuse_cosyvoice_file(target.path(), staging.path(), &file, &token).is_err());
        assert_eq!(fs::read(path).unwrap(), b"abc");
    }

    #[test]
    fn failed_publish_restores_partial_install() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("cosyvoice3");
        let staged = root.path().join("staged");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("cosyvoice3.yaml"), b"{}").unwrap();
        assert!(publish_model(&staged, &target).is_err());
        assert_eq!(fs::read(target.join("cosyvoice3.yaml")).unwrap(), b"{}");
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
