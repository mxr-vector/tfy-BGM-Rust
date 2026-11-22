//! 「基本系统配置 → 模型」：ASR 模型目录、扫描、下载与 sherpa-onnx 推理。
//!
//! 技术路线固定为 sherpa-onnx：Rust 端直接调用其安全封装
//! （`OfflineRecognizer` + `OfflineQwen3ASRModelConfig`），不引入其他推理框架。
//! 当前只支持 ASR 模型，也只识别 Qwen3 ASR 的四件套目录结构：
//! `conv_frontend.onnx`、`encoder.int8.onnx`、`decoder.int8.onnx`、`tokenizer/`。
//!
//! 默认模型固定为 0.6B-int8 版本；下载清单写死为这四个构件（外加 tokenizer 内
//! 的词表文件），配置里只允许替换下载地址，不允许换成本、换参数量。

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use bgm_core::Prefs;
use bgm_core::paths::MODELS_DIR_NAME;
use parking_lot::Mutex;
use serde::Serialize;
use sherpa_onnx::{OfflineQwen3ASRModelConfig, OfflineRecognizer, OfflineRecognizerConfig, Wave};
use tauri::{AppHandle, Emitter};

/// ASR 默认模型（仅 0.6B-int8 版本）。
pub const DEFAULT_MODEL_NAME: &str = "sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25";
/// 默认下载地址：ModelScope 上的 0.6B-int8 模型目录，仅作为镜像入口。
/// 值定义在 `bgm_core::prefs`，此处复用，保证偏好层与运行时使用同一个地址。
pub const DEFAULT_DOWNLOAD_URL: &str = bgm_core::prefs::DEFAULT_ASR_DOWNLOAD_URL;
/// 下载进度事件名；前端据此刷新进度条。
pub const DOWNLOAD_EVENT: &str = "model-download-progress";
/// 推理引擎标识，界面上原样展示。
const ENGINE: &str = "sherpa-onnx";
/// 扫描目录的最大深度，避免异常目录结构拖慢界面。
const MAX_SCAN_DEPTH: usize = 4;
/// 进度事件的最小间隔，避免大文件下载时刷爆事件通道。
const PROGRESS_INTERVAL: Duration = Duration::from_millis(240);

/// Qwen3 ASR 必需的四个构件；顺序即界面上的提示顺序。
const COMPONENTS: [&str; 4] = [
    "conv_frontend.onnx",
    "encoder.int8.onnx",
    "decoder.int8.onnx",
    "tokenizer",
];

/// 下载清单：远端路径（ModelScope 仓库内）→ 本地相对路径。
///
/// 远端路径写死为 0.6B-int8 目录与仓库根部的 `tokenizer/`，因此即使使用者把
/// 下载地址换成同一仓库的其他目录，也只会取这四个构件，不会误下 fp32 或 1.7B。
struct RemoteFile {
    remote: &'static str,
    local: &'static str,
    bytes: u64,
}

const QWEN3_FILES: [RemoteFile; 9] = [
    RemoteFile {
        remote: "model_0.6B/conv_frontend.onnx",
        local: "conv_frontend.onnx",
        bytes: 44_148_281,
    },
    RemoteFile {
        remote: "model_0.6B/encoder.int8.onnx",
        local: "encoder.int8.onnx",
        bytes: 182_491_662,
    },
    RemoteFile {
        remote: "model_0.6B/decoder.int8.onnx",
        local: "decoder.int8.onnx",
        bytes: 755_914_231,
    },
    RemoteFile {
        remote: "tokenizer/merges.txt",
        local: "tokenizer/merges.txt",
        bytes: 1_671_853,
    },
    RemoteFile {
        remote: "tokenizer/vocab.json",
        local: "tokenizer/vocab.json",
        bytes: 2_776_833,
    },
    RemoteFile {
        remote: "tokenizer/tokenizer_config.json",
        local: "tokenizer/tokenizer_config.json",
        bytes: 12_487,
    },
    RemoteFile {
        remote: "tokenizer/config.json",
        local: "tokenizer/config.json",
        bytes: 6_193,
    },
    RemoteFile {
        remote: "tokenizer/chat_template.json",
        local: "tokenizer/chat_template.json",
        bytes: 1_161,
    },
    RemoteFile {
        remote: "tokenizer/preprocessor_config.json",
        local: "tokenizer/preprocessor_config.json",
        bytes: 330,
    },
];

/// 扫描到的一个 ASR 模型；目录内的 `.onnx` 统一归到同一条记录。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AsrModel {
    /// 相对模型目录的稳定标识，用作前端列表 key。
    pub id: String,
    pub name: String,
    /// 模型所在目录（根目录下的单文件模型为模型目录本身）。
    pub path: String,
    /// 目录内的 `.onnx` 文件名，按名称排序。
    pub files: Vec<String>,
    /// 模型目录总大小（含 tokenizer 等附属文件）。
    pub size_bytes: u64,
    pub is_default: bool,
    /// Qwen3 四件套是否齐全；不齐全时前端不提供「加载」。
    pub loadable: bool,
    pub missing: Vec<String>,
}

/// 默认模型（0.6B-int8）的安装状态；缺失时界面显示「下载」。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DefaultModel {
    pub name: String,
    pub path: String,
    pub installed: bool,
    pub missing: Vec<String>,
    pub size_bytes: u64,
    /// 完整下载后的体积，用于下载前展示。
    pub expected_bytes: u64,
}

/// 已加载到内存的 ASR 模型。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadedModel {
    pub name: String,
    pub path: String,
    pub engine: &'static str,
    pub threads: u32,
}

/// 「模型」分组的完整快照，前端一次读取即可渲染。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsInfo {
    pub root: String,
    /// 模型目录是否为默认位置（系统「下载」目录下的 `bgm-models/`）。
    pub is_default_root: bool,
    pub default_download_url: String,
    /// 当前生效的下载地址；自定义地址为空时等于默认地址。
    pub download_url: String,
    pub default_model: DefaultModel,
    pub models: Vec<AsrModel>,
    pub loaded: Option<LoadedModel>,
    pub downloading: bool,
    pub warnings: Vec<String>,
}

/// 下载进度事件负载；`done` 表示本次任务结束（成功或失败）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub file: String,
    pub index: usize,
    pub total_files: usize,
    pub received: u64,
    pub file_bytes: u64,
    pub overall_received: u64,
    pub overall_bytes: u64,
    pub done: bool,
    pub error: Option<String>,
}

/// 默认模型目录：系统「下载」目录下的 `bgm-models`。
///
/// Windows 取 `FOLDERID_Downloads`（用户重定向过的下载目录同样有效），
/// Linux 取 `XDG_DOWNLOAD_DIR`；两者通常都不在程序所在盘，避免大体积模型
/// 占满软件安装分区。系统未定义下载目录时退到 `<用户主目录>/Downloads`。
pub fn default_root() -> PathBuf {
    let base = dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join("Downloads")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join(MODELS_DIR_NAME)
}

/// 当前生效的模型目录：用户自定义优先，否则回退默认位置。
pub fn root_for(prefs: &Prefs) -> PathBuf {
    prefs
        .models_dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(default_root)
}

/// 目录不存在时创建；返回的错误由调用方决定是否作为告警展示。
pub fn ensure_root(root: &Path) -> Result<()> {
    fs::create_dir_all(root).with_context(|| format!("无法创建模型目录：{}", root.display()))
}

/// 扫描模型目录下所有 `.onnx`，按所在目录聚合成 ASR 模型列表。
///
/// 目录内的多个 `.onnx`（如 Qwen3 的 conv_frontend / encoder / decoder）属于同一个
/// 模型；直接放在模型根目录下的 `.onnx` 视为单文件模型，各自成条。
pub fn scan(root: &Path) -> (Vec<AsrModel>, Vec<String>) {
    let mut warnings = Vec::new();
    if !root.is_dir() {
        return (
            Vec::new(),
            vec![format!("模型目录不存在：{}", root.display())],
        );
    }

    let mut onnx_files = Vec::new();
    collect_onnx(root, 0, &mut onnx_files, &mut warnings);

    let mut groups: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for path in onnx_files {
        let parent = path.parent().unwrap_or(root).to_path_buf();
        groups.entry(parent).or_default().push(path);
    }

    let mut models = Vec::with_capacity(groups.len());
    for (dir, mut files) in groups {
        files.sort();
        let relative = dir.strip_prefix(root).unwrap_or(Path::new(""));
        let is_root = relative.as_os_str().is_empty();
        let file_names: Vec<String> = files
            .iter()
            .filter_map(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect();

        if is_root {
            // 根目录下的单文件模型：文件名即模型名，不满足 Qwen3 目录结构。
            for (path, file_name) in files.iter().zip(&file_names) {
                let name = path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_else(|| file_name.clone());
                models.push(AsrModel {
                    id: format!("file:{name}"),
                    name,
                    path: root.to_string_lossy().into_owned(),
                    files: vec![file_name.clone()],
                    size_bytes: fs::metadata(path).map(|meta| meta.len()).unwrap_or(0),
                    is_default: false,
                    loadable: false,
                    // 单文件模型本来就不满足 Qwen3 目录结构，逐项列出缺失构件只会误导。
                    missing: Vec::new(),
                });
            }
            continue;
        }

        let name = dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| relative.to_string_lossy().replace('\\', "/"));
        let missing = missing_components(&dir);
        models.push(AsrModel {
            id: relative.to_string_lossy().replace('\\', "/"),
            name,
            path: dir.to_string_lossy().into_owned(),
            files: file_names,
            size_bytes: dir_size(&dir),
            is_default: relative == Path::new(DEFAULT_MODEL_NAME),
            loadable: missing.is_empty(),
            missing,
        });
    }

    models.sort_by(|left, right| {
        right
            .is_default
            .cmp(&left.is_default)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    (models, warnings)
}

/// 默认模型的安装状态；目录存在但构件不全时同样视为未安装，下载会续传缺失文件。
pub fn default_model(root: &Path) -> DefaultModel {
    let dir = root.join(DEFAULT_MODEL_NAME);
    let exists = dir.is_dir();
    let missing = if exists {
        missing_components(&dir)
    } else {
        COMPONENTS.iter().map(|name| name.to_string()).collect()
    };
    DefaultModel {
        name: DEFAULT_MODEL_NAME.to_owned(),
        path: dir.to_string_lossy().into_owned(),
        installed: exists && missing.is_empty(),
        missing,
        size_bytes: if exists { dir_size(&dir) } else { 0 },
        expected_bytes: QWEN3_FILES.iter().map(|file| file.bytes).sum(),
    }
}

/// 已加载模型与下载任务的运行期状态；克隆只复制内部共享句柄。
#[derive(Clone, Default)]
pub struct ModelRuntime {
    loaded: Arc<Mutex<Option<Arc<Loaded>>>>,
    downloading: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
}

struct Loaded {
    model: LoadedModel,
    recognizer: OfflineRecognizer,
}

impl ModelRuntime {
    pub fn loaded(&self) -> Option<LoadedModel> {
        self.loaded
            .lock()
            .as_ref()
            .map(|loaded| loaded.model.clone())
    }

    pub fn is_downloading(&self) -> bool {
        self.downloading.load(Ordering::SeqCst)
    }

    pub fn unload(&self) -> bool {
        self.loaded.lock().take().is_some()
    }

    /// 请求中断正在进行的下载；返回是否有下载任务在运行。
    pub fn cancel_download(&self) -> bool {
        if self.downloading.load(Ordering::SeqCst) {
            self.cancelled.store(true, Ordering::SeqCst);
            true
        } else {
            false
        }
    }

    /// 用 sherpa-onnx 的 Rust API 加载 Qwen3 ASR；耗时较长，调用方应放到阻塞线程。
    pub fn load(&self, root: &Path, dir: &Path) -> Result<LoadedModel> {
        // canonical 只用于越界校验；Windows 上它会带 `\\?\` 前缀，
        // 因此仍用调用方传入的路径（来自扫描结果）作为模型标识，保证前端逐字匹配。
        let canonical_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let canonical_dir =
            fs::canonicalize(dir).with_context(|| format!("模型目录不存在：{}", dir.display()))?;
        if !canonical_dir.starts_with(&canonical_root) {
            bail!("模型必须位于当前模型目录下：{}", root.display());
        }
        let missing = missing_components(dir);
        if !missing.is_empty() {
            bail!("模型缺少必要构件：{}", missing.join("、"));
        }

        let threads = thread_count();
        let recognizer = create_recognizer(dir, threads)?;
        let name = dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| DEFAULT_MODEL_NAME.to_owned());
        let model = LoadedModel {
            name,
            path: dir.to_string_lossy().into_owned(),
            engine: ENGINE,
            threads,
        };
        *self.loaded.lock() = Some(Arc::new(Loaded {
            model: model.clone(),
            recognizer,
        }));
        Ok(model)
    }

    /// 用已加载的模型识别一个 WAV 文件；未加载模型时直接报错。
    ///
    /// 只在取模型句柄时短暂持锁，解码在锁外进行，避免识别期间阻塞
    /// `models_info` / 加载 / 释放等命令。
    pub fn transcribe(&self, wav: &Path) -> Result<String> {
        let loaded = {
            let guard = self.loaded.lock();
            guard.as_ref().cloned()
        }
        .ok_or_else(|| anyhow!("尚未加载 ASR 模型"))?;
        let filename = wav.to_string_lossy();
        let wave = Wave::read(&filename)
            .ok_or_else(|| anyhow!("无法读取音频文件（仅支持 WAV）：{}", wav.display()))?;

        let stream = loaded.recognizer.create_stream();
        stream.accept_waveform(wave.sample_rate(), wave.samples());
        loaded.recognizer.decode(&stream);
        stream
            .get_result()
            .map(|result| result.text)
            .ok_or_else(|| anyhow!("识别失败：sherpa-onnx 未返回结果"))
    }

    /// 下载默认模型（0.6B-int8）；同一时间只允许一个下载任务。
    pub fn download(&self, app: &AppHandle, root: &Path, url: &str) -> Result<()> {
        // 先清掉上一次的取消标记，再抢下载标志，避免取消请求把新任务立刻中断。
        self.cancelled.store(false, Ordering::SeqCst);
        if self
            .downloading
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            bail!("已有模型下载任务正在进行");
        }
        let result = download_files(app, root, url, &self.cancelled);
        self.downloading.store(false, Ordering::SeqCst);
        if let Err(error) = &result {
            let _ = app.emit(
                DOWNLOAD_EVENT,
                DownloadProgress {
                    file: String::new(),
                    index: 0,
                    total_files: QWEN3_FILES.len(),
                    received: 0,
                    file_bytes: 0,
                    overall_received: 0,
                    overall_bytes: total_bytes(),
                    done: true,
                    error: Some(format!("{error:#}")),
                },
            );
        }
        result
    }
}

/// 组装 `OfflineQwen3ASRModelConfig`：四个路径全部指向模型目录内的构件。
fn create_recognizer(dir: &Path, threads: u32) -> Result<OfflineRecognizer> {
    let path_of = |name: &str| dir.join(name).to_string_lossy().into_owned();
    let mut config = OfflineRecognizerConfig::default();
    config.model_config.qwen3_asr = OfflineQwen3ASRModelConfig {
        conv_frontend: Some(path_of("conv_frontend.onnx")),
        encoder: Some(path_of("encoder.int8.onnx")),
        decoder: Some(path_of("decoder.int8.onnx")),
        tokenizer: Some(dir.join("tokenizer").to_string_lossy().into_owned()),
        ..Default::default()
    };
    // Qwen3 使用 tokenizer 目录，不读取 tokens.txt；官方 Rust 示例同样传空串。
    config.model_config.tokens = Some(String::new());
    config.model_config.provider = Some("cpu".to_owned());
    config.model_config.num_threads = threads as i32;
    config.model_config.debug = false;

    OfflineRecognizer::create(&config).with_context(|| {
        format!(
            "sherpa-onnx 无法加载模型 {}（请检查 conv_frontend / encoder.int8 / decoder.int8 / tokenizer）",
            dir.display()
        )
    })
}

/// 下载地址的两种形态：ModelScope 仓库入口，或可直接拼接文件名的目录地址。
enum DownloadSource {
    ModelScope {
        owner: String,
        repo: String,
        revision: String,
    },
    Base(String),
}

impl DownloadSource {
    fn parse(url: &str) -> Result<Self> {
        let trimmed = url.trim().trim_end_matches('/');
        if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
            bail!("下载地址需以 http:// 或 https:// 开头");
        }

        // 参考地址形如 https://modelscope.cn/models/{owner}/{repo}/tree/{revision}/{path}
        // 只取仓库信息；文件清单固定为 0.6B-int8，不使用地址里的路径。
        let lower = trimmed.to_ascii_lowercase();
        if let Some(position) = lower.find("modelscope.cn/models/") {
            let tail = &trimmed[position + "modelscope.cn/models/".len()..];
            let mut parts = tail.split('/');
            let owner = parts.next().unwrap_or_default().trim().to_owned();
            let repo = parts
                .next()
                .unwrap_or_default()
                .split(['?', '#'])
                .next()
                .unwrap_or_default()
                .trim()
                .to_owned();
            if !owner.is_empty() && !repo.is_empty() {
                let rest: Vec<&str> = parts.collect();
                let revision = match rest.as_slice() {
                    ["tree", revision, ..] if !revision.is_empty() => (*revision).to_owned(),
                    _ => "master".to_owned(),
                };
                return Ok(Self::ModelScope {
                    owner,
                    repo,
                    revision,
                });
            }
        }

        Ok(Self::Base(trimmed.to_owned()))
    }

    fn file_url(&self, file: &RemoteFile) -> String {
        match self {
            Self::ModelScope {
                owner,
                repo,
                revision,
            } => format!(
                "https://modelscope.cn/api/v1/models/{owner}/{repo}/repo?Revision={revision}&FilePath={}",
                file.remote
            ),
            Self::Base(base) => format!("{base}/{}", file.local),
        }
    }
}

/// 逐文件下载到 `models/<默认模型名>/`；已完整存在的文件直接跳过，便于失败后续传。
///
/// 进度与完整性以响应的 `Content-Length` 为准（缺失时回退清单字节数），
/// 避免上游文件更新后因大小不一致而永久失败；取消或读写失败时清理 `.part`。
fn download_files(app: &AppHandle, root: &Path, url: &str, cancelled: &AtomicBool) -> Result<()> {
    let url = if url.trim().is_empty() {
        DEFAULT_DOWNLOAD_URL
    } else {
        url.trim()
    };
    let source = DownloadSource::parse(url)?;
    ensure_root(root)?;

    let target = root.join(DEFAULT_MODEL_NAME);
    fs::create_dir_all(&target)
        .with_context(|| format!("无法创建模型目录：{}", target.display()))?;

    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(60 * 60))
        .user_agent(concat!("tfy-bgm/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("初始化下载客户端失败")?;

    let expected = total_bytes();
    let mut overall = 0u64;
    for file in &QWEN3_FILES {
        if is_complete(&target.join(file.local), file) {
            overall += file.bytes;
        }
    }

    for (index, file) in QWEN3_FILES.iter().enumerate() {
        if cancelled.load(Ordering::SeqCst) {
            bail!("下载已取消");
        }
        let path = target.join(file.local);
        if is_complete(&path, file) {
            emit_progress(
                app,
                DownloadProgress {
                    file: file.local.to_owned(),
                    index,
                    total_files: QWEN3_FILES.len(),
                    received: file.bytes,
                    file_bytes: file.bytes,
                    overall_received: overall,
                    overall_bytes: expected,
                    done: true,
                    error: None,
                },
            );
            continue;
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("无法创建目录：{}", parent.display()))?;
        }
        let file_url = source.file_url(file);
        let mut response = client
            .get(&file_url)
            .send()
            .with_context(|| format!("请求下载地址失败：{file_url}"))?;
        if !response.status().is_success() {
            bail!(
                "下载 {} 失败：HTTP {}（{file_url}）",
                file.local,
                response.status()
            );
        }
        if response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().contains("text/html"))
        {
            bail!("下载地址返回的是网页而不是模型文件，请填写模型文件目录或可直连的地址");
        }

        let expected_len = response.content_length().unwrap_or(file.bytes);
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| file.local.to_owned());
        let part = path.with_file_name(format!("{file_name}.part"));
        let mut writer =
            File::create(&part).with_context(|| format!("无法写入临时文件：{}", part.display()))?;

        let mut received = 0u64;
        let mut last_emit = Instant::now()
            .checked_sub(PROGRESS_INTERVAL)
            .unwrap_or_else(Instant::now);
        let mut buffer = vec![0u8; 256 * 1024];
        let mut copy_error: Option<anyhow::Error> = None;
        loop {
            if cancelled.load(Ordering::SeqCst) {
                copy_error = Some(anyhow!("下载已取消"));
                break;
            }
            let read = match response.read(&mut buffer) {
                Ok(read) => read,
                Err(error) => {
                    copy_error = Some(anyhow!("下载 {} 失败：{error}", file.local));
                    break;
                }
            };
            if read == 0 {
                break;
            }
            if let Err(error) = writer.write_all(&buffer[..read]) {
                copy_error = Some(anyhow!("写入 {} 失败：{error}", part.display()));
                break;
            }
            received += read as u64;
            if last_emit.elapsed() >= PROGRESS_INTERVAL {
                emit_progress(
                    app,
                    DownloadProgress {
                        file: file.local.to_owned(),
                        index,
                        total_files: QWEN3_FILES.len(),
                        received,
                        file_bytes: expected_len,
                        overall_received: overall + received,
                        overall_bytes: expected,
                        done: false,
                        error: None,
                    },
                );
                last_emit = Instant::now();
            }
        }
        writer.flush().ok();
        drop(writer);

        if let Some(error) = copy_error {
            let _ = fs::remove_file(&part);
            return Err(error);
        }
        if received == 0 {
            let _ = fs::remove_file(&part);
            bail!("下载 {} 失败：服务器返回空文件", file.local);
        }
        if expected_len > 0 && received < expected_len {
            let _ = fs::remove_file(&part);
            bail!(
                "下载 {} 不完整：已收到 {} 字节，期望 {} 字节",
                file.local,
                received,
                expected_len
            );
        }
        if path.exists() {
            let _ = fs::remove_file(&path);
        }
        fs::rename(&part, &path)
            .with_context(|| format!("无法保存模型文件：{}", path.display()))?;

        overall += received;
        emit_progress(
            app,
            DownloadProgress {
                file: file.local.to_owned(),
                index,
                total_files: QWEN3_FILES.len(),
                received,
                file_bytes: expected_len,
                overall_received: overall,
                overall_bytes: expected,
                done: true,
                error: None,
            },
        );
    }

    let missing = missing_components(&target);
    if !missing.is_empty() {
        bail!("下载完成后仍缺少构件：{}", missing.join("、"));
    }
    Ok(())
}

fn emit_progress(app: &AppHandle, progress: DownloadProgress) {
    let _ = app.emit(DOWNLOAD_EVENT, progress);
}

fn total_bytes() -> u64 {
    QWEN3_FILES.iter().map(|file| file.bytes).sum()
}

fn is_complete(path: &Path, file: &RemoteFile) -> bool {
    fs::metadata(path)
        .map(|meta| meta.is_file() && meta.len() == file.bytes)
        .unwrap_or(false)
}

/// Qwen3 ASR 四件套中缺失的项；`tokenizer` 单独按目录判断。
fn missing_components(dir: &Path) -> Vec<String> {
    let mut missing = Vec::new();
    for name in [
        "conv_frontend.onnx",
        "encoder.int8.onnx",
        "decoder.int8.onnx",
    ] {
        if !dir.join(name).is_file() {
            missing.push(name.to_owned());
        }
    }
    if !dir.join("tokenizer").is_dir() {
        missing.push("tokenizer/".to_owned());
    }
    missing
}

fn collect_onnx(dir: &Path, depth: usize, out: &mut Vec<PathBuf>, warnings: &mut Vec<String>) {
    if depth > MAX_SCAN_DEPTH {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(format!("无法读取目录 {}：{error}", dir.display()));
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        // 不跟随符号链接，避免自引用目录造成无限扫描。
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            let hidden = entry.file_name().to_string_lossy().starts_with('.');
            if !hidden {
                collect_onnx(&path, depth + 1, out, warnings);
            }
        } else if is_onnx(&path) {
            out.push(path);
        }
    }
}

fn is_onnx(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("onnx"))
}

fn dir_size(dir: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = fs::read_dir(dir) else {
        return total;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            total += dir_size(&path);
        } else if let Ok(meta) = entry.metadata() {
            total += meta.len();
        }
    }
    total
}

/// 推理线程数：取可用核心数并限制上限，避免占满整机。
fn thread_count() -> u32 {
    std::thread::available_parallelism()
        .map(|count| count.get().min(4) as u32)
        .unwrap_or(2)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
