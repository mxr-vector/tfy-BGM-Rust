//! TTS 模型清单、下载源校验、SHA256 校验安装与移除。

use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use bgm_core::prefs::{
    DEFAULT_TTS_DOWNLOAD_URL, DEFAULT_TTS_MIRROR_URL, DEFAULT_TTS_RELEASE_URL,
    TTS_COSYVOICE_MODEL_ID, TTS_SHERPA_MODEL_IDS,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use super::Progress;

pub const DIR: &str = "tts";
const FILE_SUFFIXES: [&str; 6] = [".tar.bz2", ".onnx", ".data", ".json", ".txt", ".bin"];

const COSYVOICE_ONNX_REPO: &str = "zhangluoyang/cosyvoice3-onnx";
const COSYVOICE_ONNX_REV: &str = "4e43c04c7bb86d5551da5bc7dc912f3c326ffe30";
const COSYVOICE_OFFICIAL_REPO: &str = "FunAudioLLM/Fun-CosyVoice3-0.5B-2512";
const COSYVOICE_OFFICIAL_REV: &str = "9f9c56f2514700ef79d64fd0afb693e0d672373b";

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

pub struct CosyVoiceFile {
    pub path: &'static str,
    pub size: u64,
    pub hash: &'static str,
    pub official: bool,
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

pub const COSYVOICE_FILES: &[CosyVoiceFile] = &[
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

pub const COSYVOICE_SIZE: u64 = {
    let mut total = 0;
    let mut i = 0;
    while i < COSYVOICE_FILES.len() {
        total += COSYVOICE_FILES[i].size;
        i += 1;
    }
    total
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    CosyVoice3,
    Kokoro,
    ZipVoice,
}

impl Family {
    pub const ALL: [Self; 3] = [Self::CosyVoice3, Self::Kokoro, Self::ZipVoice];

    pub fn label(self) -> &'static str {
        match self {
            Self::CosyVoice3 => "CosyVoice3",
            Self::Kokoro => "Kokoro",
            Self::ZipVoice => "ZipVoice",
        }
    }

    pub fn subtitle(self) -> &'static str {
        match self {
            Self::CosyVoice3 => "零样本克隆",
            Self::Kokoro => "自然朗读",
            Self::ZipVoice => "声音克隆",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Model {
    #[default]
    KokoroInt8,
    Kokoro,
    ZipVoice,
    ZipVoiceFp32,
    CosyVoice3,
}

impl Model {
    pub const ALL: [Model; 5] = [
        Self::CosyVoice3,
        Self::KokoroInt8,
        Self::Kokoro,
        Self::ZipVoice,
        Self::ZipVoiceFp32,
    ];

    pub fn by_family(family: Family) -> Vec<Self> {
        Self::ALL
            .into_iter()
            .filter(|model| model.family() == family)
            .collect()
    }

    pub fn family(self) -> Family {
        match self {
            Self::CosyVoice3 => Family::CosyVoice3,
            Self::KokoroInt8 | Self::Kokoro => Family::Kokoro,
            Self::ZipVoice | Self::ZipVoiceFp32 => Family::ZipVoice,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::CosyVoice3 => TTS_COSYVOICE_MODEL_ID,
            Self::KokoroInt8 => TTS_SHERPA_MODEL_IDS[0],
            Self::Kokoro => TTS_SHERPA_MODEL_IDS[1],
            Self::ZipVoice => TTS_SHERPA_MODEL_IDS[2],
            Self::ZipVoiceFp32 => TTS_SHERPA_MODEL_IDS[3],
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|model| model.key() == key)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::CosyVoice3 => "CosyVoice3 · 0.5B",
            Self::KokoroInt8 => "Kokoro · INT8",
            Self::Kokoro => "Kokoro · FP32",
            Self::ZipVoice => "ZipVoice · INT8",
            Self::ZipVoiceFp32 => "ZipVoice · FP32",
        }
    }

    pub fn recommended(self) -> bool {
        matches!(self, Self::CosyVoice3 | Self::KokoroInt8 | Self::ZipVoice)
    }

    pub fn package(self) -> &'static str {
        match self {
            Self::CosyVoice3 => "cosyvoice3",
            Self::KokoroInt8 => "kokoro-int8-multi-lang-v1_1.tar.bz2",
            Self::Kokoro => "kokoro-multi-lang-v1_1.tar.bz2",
            Self::ZipVoice => "sherpa-onnx-zipvoice-distill-int8-zh-en-emilia.tar.bz2",
            Self::ZipVoiceFp32 => "sherpa-onnx-zipvoice-distill-fp32-zh-en-emilia.tar.bz2",
        }
    }

    pub fn package_dir(self) -> &'static str {
        self.package()
            .strip_suffix(".tar.bz2")
            .unwrap_or(self.package())
    }

    pub fn size_bytes(self) -> u64 {
        match self {
            Self::CosyVoice3 => COSYVOICE_SIZE,
            Self::KokoroInt8 => 147_052_523,
            Self::Kokoro => 364_855_331,
            Self::ZipVoice => 109_162_785,
            Self::ZipVoiceFp32 => 477_800_463,
        }
    }

    pub fn hash(self) -> &'static str {
        match self {
            Self::CosyVoice3 => "",
            Self::KokoroInt8 => "259c9c8bc906d6078e4e0570421885dabb68b539c2f970fcbb890a58eeaf3e9c",
            Self::Kokoro => "b78e1ac42b852e839cceaafbe88f66806bf5860992d1d7b9a3f2ad1198427aed",
            Self::ZipVoice => "77219c8b40f4ee8d73a7f902305ff6c1128ef9b54461c41b4ca6ed890b6c2803",
            Self::ZipVoiceFp32 => {
                "3b6729d03bf4ba64deeec113048bdbbe55dbc30580b609be76342e0099fd23a8"
            }
        }
    }

    pub fn uses_reference(self) -> bool {
        matches!(self, Self::ZipVoice | Self::ZipVoiceFp32 | Self::CosyVoice3)
    }

    pub fn default_browse_url(self) -> &'static str {
        if self == Self::CosyVoice3 {
            DEFAULT_TTS_DOWNLOAD_URL
        } else {
            DEFAULT_TTS_RELEASE_URL
        }
    }

    pub fn default_source_url(self) -> &'static str {
        if self == Self::CosyVoice3 {
            DEFAULT_TTS_DOWNLOAD_URL
        } else {
            DEFAULT_TTS_MIRROR_URL
        }
    }

    pub fn dir(self, models_root: &Path) -> PathBuf {
        models_root.join(DIR).join(self.package_dir())
    }

    pub fn model_file(self, directory: &Path) -> &'static str {
        if self == Self::KokoroInt8 && regular_file(&directory.join("model.int8.onnx")) {
            "model.int8.onnx"
        } else {
            "model.onnx"
        }
    }

    pub fn encoder(self) -> &'static str {
        if self == Self::ZipVoice {
            "encoder.int8.onnx"
        } else {
            "encoder.onnx"
        }
    }

    pub fn decoder(self) -> &'static str {
        if self == Self::ZipVoice {
            "decoder.int8.onnx"
        } else {
            "decoder.onnx"
        }
    }

    pub fn check(self, directory: &Path) -> Result<(), String> {
        if !fs::symlink_metadata(directory).is_ok_and(|meta| meta.is_dir()) {
            return Err(format!("未找到模型目录 {}", directory.display()));
        }
        if self == Self::CosyVoice3 {
            let missing: Vec<_> = COSYVOICE_FILES
                .iter()
                .filter(|file| {
                    !fs::metadata(directory.join(file.path))
                        .is_ok_and(|info| info.is_file() && info.len() == file.size)
                })
                .map(|file| file.path)
                .collect();
            return if missing.is_empty() {
                Ok(())
            } else {
                Err(format!("模型文件缺失或未校验：{}", missing.join("、")))
            };
        }
        let required: Vec<&'static str> = match self.family() {
            Family::Kokoro => vec![
                if self == Self::KokoroInt8 {
                    "model.int8.onnx"
                } else {
                    "model.onnx"
                },
                "voices.bin",
                "tokens.txt",
                "lexicon-us-en.txt",
                "lexicon-zh.txt",
            ],
            Family::ZipVoice => vec![self.encoder(), self.decoder(), "tokens.txt", "lexicon.txt"],
            Family::CosyVoice3 => unreachable!(),
        };
        let missing: Vec<_> = required
            .into_iter()
            .filter(|file| {
                if self == Self::KokoroInt8 && *file == "model.int8.onnx" {
                    !regular_file(&directory.join("model.int8.onnx"))
                        && !regular_file(&directory.join("model.onnx"))
                } else {
                    !regular_file(&directory.join(file))
                }
            })
            .collect();
        if !missing.is_empty() {
            return Err(format!("模型文件缺失：{}", missing.join("、")));
        }
        if self.family() == Family::ZipVoice {
            let tts_root = directory.parent().unwrap_or(directory);
            let missing_aux: Vec<_> = AUXILIARY
                .iter()
                .filter(|(file, _, _, _)| {
                    !regular_file(&tts_root.join(file)) && !regular_file(&directory.join(file))
                })
                .map(|(file, _, _, _)| *file)
                .collect();
            if !missing_aux.is_empty() {
                return Err(format!("缺少共享语音依赖：{}", missing_aux.join("、")));
            }
        }
        Ok(())
    }

    pub fn aux_path(directory: &Path, name: &str) -> PathBuf {
        let shared = directory.parent().unwrap_or(directory).join(name);
        if regular_file(&shared) {
            shared
        } else {
            directory.join(name)
        }
    }
}

fn regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() > 0)
}

enum DownloadSource<'a> {
    Default,
    ModelScope { origin: String, repo: String },
    Mirror(&'a str),
}

impl<'a> DownloadSource<'a> {
    fn parse(source: &'a str) -> Result<Self, String> {
        if source.is_empty() {
            return Ok(Self::Default);
        }
        let url = Url::parse(source).map_err(|_| "无效的 TTS 下载源".to_owned())?;
        if !is_modelscope_repo(&url) {
            return Ok(Self::Mirror(source));
        }
        let parts: Vec<_> = url.path().trim_end_matches('/').split('/').collect();
        let (owner, name) = match parts.as_slice() {
            ["", "models", owner, name] | ["", "models", owner, name, "files"] => (*owner, *name),
            _ => return Err("请输入 ModelScope 模型仓库首页或 /files 地址".into()),
        };
        if ![owner, name].iter().all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        }) {
            return Err("ModelScope 模型仓库名称无效".into());
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

    fn cosyvoice_url(&self, file: &CosyVoiceFile) -> Result<String, String> {
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
        let mut url = Url::parse(&format!("{origin}/api/v1/models/{repo}/repo"))
            .map_err(|error| error.to_string())?;
        url.query_pairs_mut()
            .append_pair("Revision", revision)
            .append_pair("FilePath", file.path);
        Ok(url.into())
    }
}

fn is_modelscope_repo(url: &Url) -> bool {
    matches!(url.host_str(), Some("modelscope.cn" | "www.modelscope.cn"))
        && (url.path() == "/models" || url.path().starts_with("/models/"))
}

pub fn validate_source(model: Model, source: &str) -> Result<(), String> {
    let source = source.trim().trim_end_matches('/');
    if source.is_empty() {
        return Ok(());
    }
    let url = Url::parse(source).map_err(|_| "请输入有效的 HTTP 或 HTTPS 地址".to_owned())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("请输入有效的 HTTP 或 HTTPS 地址".into());
    }
    if model.family() == Family::CosyVoice3 {
        if !is_modelscope_repo(&url) {
            return Err("CosyVoice3 下载源需为 ModelScope 模型仓库地址（例如 .../files）".into());
        }
        DownloadSource::parse(source)?;
        return Ok(());
    }
    if is_modelscope_repo(&url) {
        return Err("Kokoro / ZipVoice 下载源需为可直接拼接文件名的 HTTP(S) 镜像目录".into());
    }
    if url.path().contains("/releases/tag/") {
        return Err("请填写可直接下载的镜像目录，而不是 GitHub release 页面".into());
    }
    let lower = source.to_ascii_lowercase();
    if FILE_SUFFIXES.iter().any(|suffix| lower.ends_with(suffix)) {
        return Err("请填写镜像目录基地址，而不是具体文件地址".into());
    }
    DownloadSource::parse(source)?;
    Ok(())
}

pub fn install(
    models_root: &Path,
    model: Model,
    source: &str,
    progress: &Progress,
) -> Result<(), String> {
    progress.ensure_running()?;
    let source = source.trim().trim_end_matches('/');
    validate_source(model, source)?;
    let root = models_root.join(DIR);
    fs::create_dir_all(&root).map_err(|error| format!("无法创建模型目录：{error}"))?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join(".install.lock"))
        .map_err(|error| error.to_string())?;
    lock.try_lock()
        .map_err(|_| "有另一个实例正在调整语音模型")?;

    let target = model.dir(models_root);
    if model.check(&target).is_ok() {
        return Ok(());
    }
    let parsed = DownloadSource::parse(source)?;
    if model == Model::CosyVoice3 {
        return install_cosyvoice3(&root, &target, &parsed, progress);
    }

    let mirror = parsed.legacy_mirror();
    if model.family() == Family::ZipVoice {
        for &(file, url, hash, size) in AUXILIARY {
            progress.ensure_running()?;
            let dest = root.join(file);
            if verify_existing(&dest, hash, size).is_ok() {
                continue;
            }
            let mirror_url;
            let url = if mirror.is_empty() {
                url
            } else {
                mirror_url = format!("{mirror}/{file}");
                &mirror_url
            };
            progress.set_download(format!("正在下载 {file}"), 0, size);
            let tmp = download_verified(&root, file, url, hash, size, 0, size, progress)?;
            progress.ensure_running()?;
            if dest.exists() {
                fs::remove_file(&dest)
                    .map_err(|error| format!("无法替换校验失败的依赖文件 {file}：{error}"))?;
            }
            tmp.persist(&dest).map_err(|error| error.to_string())?;
        }
    }

    progress.ensure_running()?;
    let archive_url = format!(
        "{}/{}",
        if mirror.is_empty() {
            DEFAULT_TTS_MIRROR_URL
        } else {
            mirror
        },
        model.package()
    );
    progress.set_download(format!("正在下载 {}", model.label()), 0, model.size_bytes());
    let archive = download_verified(
        &root,
        model.package(),
        &archive_url,
        model.hash(),
        model.size_bytes(),
        0,
        model.size_bytes(),
        progress,
    )?;

    let staging = tempfile::tempdir_in(&root).map_err(|error| error.to_string())?;
    let unpack = staging.path().join("unpacked");
    fs::create_dir(&unpack).map_err(|error| error.to_string())?;
    progress.set("正在解包并校验模型文件");
    let mut tar = Command::new("/usr/bin/tar");
    tar.arg("-xf")
        .arg(archive.path())
        .arg("-C")
        .arg(&unpack)
        .args(["--no-same-owner", "--no-same-permissions"]);
    run(
        tar,
        None,
        0,
        model.size_bytes(),
        progress,
        Duration::from_secs(300),
    )?;

    let staged = fs::read_dir(&unpack)
        .map_err(|error| error.to_string())?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .chain(std::iter::once(unpack.clone()))
        .find(|dir| dir.is_dir() && files_complete(model, dir))
        .ok_or("模型压缩包缺少推理文件")?;
    progress.ensure_running()?;
    publish_model(&staged, &target)
}

fn files_complete(model: Model, directory: &Path) -> bool {
    match model.family() {
        Family::Kokoro => [
            model.model_file(directory),
            "voices.bin",
            "tokens.txt",
            "lexicon-us-en.txt",
            "lexicon-zh.txt",
        ]
        .iter()
        .all(|name| regular_file(&directory.join(name))),
        Family::ZipVoice => [
            model.encoder(),
            model.decoder(),
            "tokens.txt",
            "lexicon.txt",
        ]
        .iter()
        .all(|name| regular_file(&directory.join(name))),
        Family::CosyVoice3 => model.check(directory).is_ok(),
    }
}

fn install_cosyvoice3(
    root: &Path,
    target: &Path,
    source: &DownloadSource<'_>,
    progress: &Progress,
) -> Result<(), String> {
    let staging = tempfile::tempdir_in(root).map_err(|error| error.to_string())?;
    let mut received = 0u64;
    for (index, file) in COSYVOICE_FILES.iter().enumerate() {
        progress.ensure_running()?;
        let destination = staging.path().join(file.path);
        fs::create_dir_all(destination.parent().ok_or("模型文件路径无效")?)
            .map_err(|error| error.to_string())?;
        progress.set_download(
            format!(
                "CosyVoice3 ({}/{}) · {}",
                index + 1,
                COSYVOICE_FILES.len(),
                file.path
            ),
            received,
            COSYVOICE_SIZE,
        );
        let temporary = match reuse_cosyvoice_file(target, staging.path(), file, progress)? {
            Some(verified) => verified,
            None => {
                let url = source.cosyvoice_url(file)?;
                download_verified(
                    staging.path(),
                    file.path,
                    &url,
                    file.hash,
                    file.size,
                    received,
                    COSYVOICE_SIZE,
                    progress,
                )?
            }
        };
        temporary
            .persist(&destination)
            .map_err(|error| error.to_string())?;
        received += file.size;
        progress.set_bytes(received, COSYVOICE_SIZE);
    }
    Model::CosyVoice3.check(staging.path())?;
    progress.ensure_running()?;
    publish_model(staging.path(), target)
}

fn reuse_cosyvoice_file(
    target: &Path,
    staging: &Path,
    file: &CosyVoiceFile,
    progress: &Progress,
) -> Result<Option<tempfile::NamedTempFile>, String> {
    let source = target.join(file.path);
    if !source
        .symlink_metadata()
        .is_ok_and(|info| info.is_file() && info.len() == file.size)
    {
        return Ok(None);
    }
    let Ok(mut reader) = fs::File::open(source) else {
        return Ok(None);
    };
    let mut output = tempfile::NamedTempFile::new_in(staging).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 128 * 1024];
    let mut size = 0u64;
    loop {
        progress.ensure_running()?;
        let count = match reader.read(&mut buffer) {
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
        output
            .write_all(&buffer[..count])
            .map_err(|error| error.to_string())?;
    }
    if size == file.size && format!("{:x}", hasher.finalize()) == file.hash {
        Ok(Some(output))
    } else {
        Ok(None)
    }
}

fn publish_model(staged: &Path, target: &Path) -> Result<(), String> {
    match target.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return fs::rename(staged, target)
                .map_err(|error| format!("安装 TTS 模型失败：{error}"));
        }
        Ok(info)
            if info.is_dir()
                && target.canonicalize().ok()
                    == target
                        .parent()
                        .and_then(|parent| parent.canonicalize().ok())
                        .zip(target.file_name())
                        .map(|(parent, name)| parent.join(name)) => {}
        Ok(_) => return Err("现有模型路径不是普通目录，无法替换".into()),
        Err(error) => return Err(format!("无法检查现有模型目录：{error}")),
    }
    let parent = target.parent().ok_or("模型目录无效")?;
    let backup_dir = tempfile::Builder::new()
        .prefix(".incomplete-")
        .tempdir_in(parent)
        .map_err(|error| error.to_string())?;
    let backup = backup_dir.path().join("model");
    fs::rename(target, &backup)
        .map_err(|error| format!("无法移开原有的不完整模型目录：{error}"))?;
    if let Err(error) = fs::rename(staged, target) {
        if let Err(restore) = fs::rename(&backup, target) {
            let kept = backup_dir.keep();
            return Err(format!(
                "安装失败且无法恢复原模型，原文件保存在 {}：{restore}",
                kept.display()
            ));
        }
        return Err(format!("安装 TTS 模型失败：{error}"));
    }
    Ok(())
}

pub fn remove(models_root: &Path, model: Model) -> Result<(), String> {
    let root = models_root.join(DIR);
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join(".install.lock"))
        .map_err(|error| error.to_string())?;
    lock.try_lock()
        .map_err(|_| "有另一个实例正在调整语音模型")?;
    let dir = model.dir(models_root);
    let target = match dir.canonicalize() {
        Ok(target) => target,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err("模型目录不存在或无效".into());
        }
        Err(error) => return Err(format!("无法定位 TTS 模型目录：{error}")),
    };
    let expected = root
        .canonicalize()
        .map_err(|error| error.to_string())?
        .join(dir.file_name().ok_or("模型目录无效")?);
    if target != expected {
        return Err("TTS 模型目录指向链接，无法删除".into());
    }
    let displaced = tempfile::tempdir_in(&root).map_err(|error| error.to_string())?;
    fs::rename(&target, displaced.path().join("model")).map_err(|error| error.to_string())?;
    displaced
        .close()
        .map_err(|error| format!("清理模型文件失败：{error}"))
}

fn verify_existing(path: &Path, expected_hash: &str, expected_size: u64) -> Result<(), String> {
    let meta = fs::metadata(path).map_err(|error| error.to_string())?;
    if !meta.is_file() || meta.len() != expected_size {
        return Err("文件大小不匹配".into());
    }
    verify_sha256(path, expected_hash, expected_size)
}

pub(super) fn verify_sha256(
    path: &Path,
    expected_hash: &str,
    expected_size: u64,
) -> Result<(), String> {
    let mut file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 128 * 1024];
    let mut size = 0u64;
    loop {
        let count = file.read(&mut buf).map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        size += count as u64;
        hasher.update(&buf[..count]);
    }
    if size != expected_size {
        return Err(format!(
            "下载文件大小异常（期望 {expected_size} 字节，实际 {size} 字节）"
        ));
    }
    if !expected_hash.is_empty() && format!("{:x}", hasher.finalize()) != expected_hash {
        return Err("下载文件 SHA256 校验失败，请重试".into());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn download_verified(
    dir: &Path,
    label: &str,
    url: &str,
    expected_hash: &str,
    expected_size: u64,
    base_bytes: u64,
    total_bytes: u64,
    progress: &Progress,
) -> Result<tempfile::NamedTempFile, String> {
    let tmp = tempfile::NamedTempFile::new_in(dir).map_err(|error| error.to_string())?;
    download_with_offset(url, tmp.path(), base_bytes, total_bytes, progress)?;
    verify_sha256(tmp.path(), expected_hash, expected_size)
        .map_err(|error| format!("{label} 校验失败：{error}"))?;
    Ok(tmp)
}

pub(super) fn download(
    source: &str,
    destination: &Path,
    progress: &Progress,
) -> Result<(), String> {
    download_with_offset(source, destination, 0, 0, progress)
}

fn download_with_offset(
    source: &str,
    destination: &Path,
    base_bytes: u64,
    total_bytes: u64,
    progress: &Progress,
) -> Result<(), String> {
    let mut curl = Command::new("/usr/bin/curl");
    curl.args([
        "--disable",
        "--silent",
        "--show-error",
        "--fail",
        "--location",
        "--max-redirs",
        "8",
        "--proto",
        "=http,https",
        "--proto-redir",
        "=http,https",
        "--connect-timeout",
        "20",
        "--speed-limit",
        "1024",
        "--speed-time",
        "60",
        "--max-filesize",
        "3221225472",
        "--output",
    ])
    .arg(destination)
    .arg("--")
    .arg(source);
    run(
        curl,
        Some(destination),
        base_bytes,
        total_bytes,
        progress,
        Duration::from_secs(7200),
    )
}

pub(super) fn run(
    mut command: Command,
    size: Option<&Path>,
    base_bytes: u64,
    total_bytes: u64,
    progress: &Progress,
    timeout: Duration,
) -> Result<(), String> {
    progress.ensure_running()?;
    let mut stderr = tempfile::tempfile().map_err(|error| error.to_string())?;
    let child_stderr = stderr.try_clone().map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(child_stderr)
        .spawn()
        .map_err(|error| error.to_string())?;
    let start = Instant::now();
    loop {
        let state = progress.ensure_running().and_then(|()| {
            if start.elapsed() >= timeout {
                Err("模型安装超时".into())
            } else {
                Ok(())
            }
        });
        if let Err(error) = state {
            stop_command(&mut child);
            return Err(error);
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                let mut detail = Vec::new();
                if let Ok(meta) = stderr.metadata()
                    && stderr
                        .seek(SeekFrom::Start(meta.len().saturating_sub(2048)))
                        .is_ok()
                {
                    let _ = stderr.take(2048).read_to_end(&mut detail);
                }
                let detail = String::from_utf8_lossy(&detail);
                return Err(format!(
                    "{}失败（{status}）{}",
                    progress.text(),
                    if detail.trim().is_empty() {
                        String::new()
                    } else {
                        format!("：{}", detail.trim())
                    }
                ));
            }
            Ok(None) => {
                if let Some(path) = size {
                    let bytes = fs::metadata(path).map_or(0, |metadata| metadata.len());
                    progress.set_bytes(base_bytes.saturating_add(bytes), total_bytes);
                }
                std::thread::sleep(Duration::from_millis(80));
            }
            Err(error) => {
                stop_command(&mut child);
                return Err(error.to_string());
            }
        }
    }
}

fn stop_command(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        if let Ok(pid) = i32::try_from(child.id()) {
            unsafe {
                kill(-pid, 9);
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_validation_is_family_aware() {
        assert!(validate_source(Model::CosyVoice3, "").is_ok());
        assert!(validate_source(Model::CosyVoice3, DEFAULT_TTS_DOWNLOAD_URL).is_ok());
        assert!(validate_source(Model::CosyVoice3, "https://mirror.example.com/tts").is_err());
        assert!(
            validate_source(
                Model::CosyVoice3,
                "https://www.modelscope.cn/models/owner/repo/files"
            )
            .is_ok()
        );

        assert!(validate_source(Model::KokoroInt8, "").is_ok());
        assert!(validate_source(Model::KokoroInt8, "https://mirror.example.com/tts").is_ok());
        assert!(validate_source(Model::KokoroInt8, "https://mirror.example.com/tts/").is_ok());
        assert!(validate_source(Model::KokoroInt8, DEFAULT_TTS_DOWNLOAD_URL).is_err());
        assert!(
            validate_source(
                Model::KokoroInt8,
                "https://mirror.example.com/tts/kokoro.tar.bz2"
            )
            .is_err()
        );
        assert!(validate_source(Model::KokoroInt8, DEFAULT_TTS_RELEASE_URL).is_err());
    }

    #[test]
    fn reused_cosyvoice_files_must_match_digest() {
        let target = tempfile::tempdir().unwrap();
        let staging = tempfile::tempdir().unwrap();
        let progress = Progress::default();
        let file = official(
            "dependency.onnx",
            3,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        );
        let path = target.path().join(file.path);
        fs::write(&path, b"bad").unwrap();
        assert!(
            reuse_cosyvoice_file(target.path(), staging.path(), &file, &progress)
                .unwrap()
                .is_none()
        );
        fs::write(&path, b"abc").unwrap();
        let verified = reuse_cosyvoice_file(target.path(), staging.path(), &file, &progress)
            .unwrap()
            .unwrap();
        assert_eq!(fs::read(verified.path()).unwrap(), b"abc");
        progress.cancel();
        assert!(reuse_cosyvoice_file(target.path(), staging.path(), &file, &progress).is_err());
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
}
