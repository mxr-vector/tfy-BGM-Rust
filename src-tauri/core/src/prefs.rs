//! 用户偏好：收藏、音量、倍速、播放模式、排序、导出响度与界面壁纸。
//!
//! 所有来自前端的数据都会经过 `normalize`，落盘采用「临时文件 + 重命名」，
//! 损坏的偏好文件回退为默认值而不是让应用启动失败。

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::limits::{MAX_FAVORITES, MAX_KEY_LEN};
use crate::loudness::find_preset;
use crate::paths::strip_verbatim_prefix;
use crate::persist::{PersistError, write_json};

pub const PREFS_VERSION: u32 = 5;
pub const DEFAULT_VOLUME: f32 = 0.85;
pub const PLAYBACK_RATES: [f32; 5] = [0.75, 1.0, 1.25, 1.5, 2.0];
/// 切歌交叉淡入淡出的默认时长（秒）；`0` 表示硬切。
pub const DEFAULT_CROSSFADE_SECONDS: f32 = 2.0;
/// 交叉淡入淡出的最大时长（秒）。
pub const MAX_CROSSFADE_SECONDS: f32 = 12.0;
/// 同时启用的输出设备上限，避免异常配置创建过多 AudioContext。
pub const MAX_OUTPUT_ROUTES: usize = 8;

/// 界面字体：`system` 表示应用内置字体栈，其余为系统字体族名。
pub const DEFAULT_UI_FONT: &str = "system";
/// 字体族名长度上限，避免异常配置写入超长字符串。
const MAX_UI_FONT_LEN: usize = 128;
/// 自定义模型目录长度上限。
const MAX_MODELS_DIR_LEN: usize = 512;
/// 模型下载地址长度上限。
const MAX_DOWNLOAD_URL_LEN: usize = 1024;
/// 分离模型 id 长度上限。
const MAX_SEPARATION_MODEL_LEN: usize = 256;
/// 按模型保存下载源时的模型 id 长度上限。
const MAX_TTS_MODEL_ID_LEN: usize = 128;
/// 按模型保存的 TTS 下载源数量上限，避免异常配置无限增长。
const MAX_TTS_DOWNLOAD_SOURCES: usize = 8;
/// ASR 模型默认下载地址：ModelScope 上的 0.6B-int8 模型目录。
/// 偏好里留空即表示使用此地址，下载地址的唯一真源放在 core，避免多处定义漂移。
pub const DEFAULT_ASR_DOWNLOAD_URL: &str =
    "https://modelscope.cn/models/zengshuishui/Qwen3-ASR-onnx/tree/master/model_0.6B";
/// CosyVoice3 ONNX 模型默认下载仓库。
pub const DEFAULT_TTS_DOWNLOAD_URL: &str =
    "https://www.modelscope.cn/models/zhangluoyang/cosyvoice3-onnx/files";
/// ZipVoice 默认浏览页；仅用于「访问下载源」，不能拼接文件名下载。
pub const DEFAULT_TTS_RELEASE_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/tag/tts-models";
/// ZipVoice 默认下载基址；自定义镜像需保持同样的「目录 + 文件名」结构。
pub const DEFAULT_TTS_MIRROR_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models";
/// CUDA 环境安装默认使用的 PyPI 镜像源（清华）。
pub const DEFAULT_CUDA_PYPI_MIRROR: &str = "https://pypi.tuna.tsinghua.edu.cn/simple";
/// CUDA 环境安装默认使用的 PyTorch 轮子镜像源（南京大学）。
pub const DEFAULT_CUDA_PYTORCH_MIRROR: &str = "https://mirrors.nju.edu.cn/pytorch/";
/// CosyVoice3 的稳定模型 id，与运行时 `tts_models::SPECS` 保持一致。
pub const TTS_COSYVOICE_MODEL_ID: &str = "cosyvoice3-0.5b-2512";
/// 走 sherpa-onnx release（压缩包 + 平铺镜像）的 TTS 模型 id。
pub const TTS_SHERPA_MODEL_IDS: [&str; 2] = ["zipvoice-int8", "zipvoice-fp32"];
/// 免下载的在线朗读引擎 id：走 Microsoft Edge Read Aloud，不占用模型目录。
pub const TTS_EDGE_MODEL_ID: &str = "edge-tts";

/// 界面壁纸允许的扩展名（图片优先，MP4 作为动态壁纸）。
pub const WALLPAPER_EXTENSIONS: [&str; 5] = ["jpg", "jpeg", "png", "webp", "mp4"];
/// 未选择壁纸时的默认样式（沿用应用自身的墨绿底色）。
pub const DEFAULT_WALLPAPER: &str = "default";
/// 界面透明度默认值（0.0 ~ 1.0，0.0 为完全实底，1.0 为最大通透）。
pub const DEFAULT_WALLPAPER_OPACITY: f32 = 0.35;
/// 界面毛玻璃模糊默认值（px，0 为完全不模糊）。
pub const DEFAULT_WALLPAPER_BLUR: u32 = 0;
/// 壁纸填充方式：`cover` 铺满裁剪，`contain` 完整展示。
pub const DEFAULT_WALLPAPER_FIT: &str = "cover";
/// 渐变底色壁纸的合法 id；图片 / 视频壁纸由资源目录自动发现，
/// id 形如 `builtin-<文件名>`，无需在此维护固定清单。
pub const GRADIENT_WALLPAPER_IDS: [&str; 3] = ["gradient-blue", "gradient-green", "gradient-red"];
/// 自动发现壁纸 id 中文件名部分的最大长度。
const MAX_BUILTIN_ID_SUFFIX_LEN: usize = 64;

/// 是否是可识别的内置壁纸 id；`custom` 需要搭配自定义文件路径才生效。
fn is_builtin_wallpaper_id(id: &str) -> bool {
    if GRADIENT_WALLPAPER_IDS.contains(&id) {
        return true;
    }
    id.strip_prefix("builtin-").is_some_and(|suffix| {
        !suffix.is_empty()
            && suffix.chars().count() <= MAX_BUILTIN_ID_SUFFIX_LEN
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    })
}

/// 自定义壁纸路径是否带受支持的扩展名（不检查文件是否存在，由调用方决定）。
pub fn is_wallpaper_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            WALLPAPER_EXTENSIONS
                .iter()
                .any(|allowed| ext.eq_ignore_ascii_case(allowed))
        })
}

/// 壁纸类型：`video` 走动态壁纸，其余一律按静态图片处理。
///
/// 视频壁纸在 Linux 上需要额外的本地媒体服务，平台实现与调用方都以此判断，
/// 因此判定规则放在这里，避免多处各写一份扩展名比较。
pub fn wallpaper_kind(path: &Path) -> &'static str {
    let is_video = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mp4"));
    if is_video { "video" } else { "image" }
}

/// 模型下载地址是否是可用的 http(s) 链接；不合法时回退内置地址。
pub fn is_download_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://"))
        && url.chars().count() <= MAX_DOWNLOAD_URL_LEN
        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlayMode {
    #[default]
    Sequence,
    RepeatOne,
    Shuffle,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SortBy {
    #[default]
    Default,
    Name,
}

/// 点击标题栏关闭按钮时的行为：每次询问 / 直接退出 / 最小化到托盘。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CloseAction {
    /// 弹出「关闭程序 / 最小化托盘」选择框。
    #[default]
    Ask,
    /// 直接退出应用。
    Quit,
    /// 隐藏到系统托盘，继续后台播放。
    Tray,
}

/// 一路输出设备：`deviceId` 来自 WebView 的 `enumerateDevices()`，`default` 表示系统默认。
///
/// 音量是这一路的独立增益，叠加在播放器主音量之上；静音只影响这一路。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OutputRoute {
    pub device_id: String,
    pub label: String,
    pub volume: f32,
    pub muted: bool,
    pub enabled: bool,
}

impl Default for OutputRoute {
    fn default() -> Self {
        Self {
            device_id: "default".to_owned(),
            label: "系统默认输出".to_owned(),
            volume: 1.0,
            muted: false,
            enabled: true,
        }
    }
}

impl OutputRoute {
    /// 把任意输入收敛为可持久化的一路输出。
    fn normalize(mut self) -> Self {
        self.device_id = self.device_id.trim().to_owned();
        self.label = self.label.trim().chars().take(64).collect();
        if self.label.is_empty() {
            self.label = "输出设备".to_owned();
        }
        self.volume = if self.volume.is_finite() {
            self.volume.clamp(0.0, 1.0)
        } else {
            1.0
        };
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Prefs {
    pub version: u32,
    pub library_dir: Option<String>,
    pub favorites: Vec<String>,
    pub volume: f32,
    pub muted: bool,
    pub playback_rate: f32,
    pub play_mode: PlayMode,
    pub sort_by: SortBy,
    /// 切歌时的交叉淡入淡出时长（秒），`0` 为硬切。
    pub crossfade_seconds: f32,
    /// 多输出设备路由；每一项都有独立音量，`default` 表示系统默认输出。
    pub output_routes: Vec<OutputRoute>,
    /// 导出时是否用 ffmpeg 做响度归一化（默认开启）。
    pub loudness_normalization: bool,
    /// 导出响度场景预设 id；空字符串表示不套用场景，使用默认人耳响度目标。
    pub export_scene: String,
    /// 当前界面壁纸 id：`default`、三个渐变色、两个内置资源或 `custom`。
    pub wallpaper: String,
    /// 上一次使用的自定义壁纸绝对路径，切换回内置壁纸后仍保留。
    pub custom_wallpaper: Option<String>,
    /// 界面透明度（0.0 ~ 1.0，0.0 为完全实底，1.0 为最大通透）。
    pub wallpaper_opacity: f32,
    /// 界面毛玻璃模糊半径（0 ~ 20 px）。
    pub wallpaper_blur: u32,
    /// 壁纸填充方式：`cover` 铺满裁剪，`contain` 完整展示。
    pub wallpaper_fit: String,

    // ---- 基本系统配置 · 常规 ----
    /// 界面字体：`system` 或系统字体族名（见 [`DEFAULT_UI_FONT`]）。
    pub ui_font: String,
    /// 开机自动启动。
    pub autostart: bool,
    /// 最小化时隐藏到系统托盘。
    pub minimize_to_tray: bool,
    /// 是否允许 WebView 使用 GPU 加速（重启应用后生效）。
    pub gpu_acceleration: bool,

    // ---- 基本系统配置 · 播放 ----
    /// 应用启动后自动开始播放。
    pub autoplay_on_start: bool,
    /// 播放时阻止系统休眠与熄屏。
    pub prevent_sleep: bool,
    /// 点击标题栏关闭按钮时的行为；`ask` 表示每次询问。
    pub close_action: CloseAction,

    // ---- 基本系统配置 · 模型 ----
    /// ASR 模型存放目录；`None` 表示系统「下载」目录下的 `bgm-models`。
    pub models_dir: Option<String>,
    /// ASR 模型下载地址；空字符串表示使用 `DEFAULT_ASR_DOWNLOAD_URL`（0.6B-int8）。
    pub asr_download_url: String,
    /// 按模型 id 覆盖的 TTS 下载源；缺失表示使用该模型内置默认源。
    pub tts_download_urls: BTreeMap<String, String>,
    /// 安装 CUDA 环境时使用的 PyPI 镜像源；空或非法时回退 `DEFAULT_CUDA_PYPI_MIRROR`。
    pub cuda_pypi_mirror: String,
    /// 安装 CUDA 环境时使用的 PyTorch 轮子镜像源；空或非法时回退 `DEFAULT_CUDA_PYTORCH_MIRROR`。
    pub cuda_pytorch_mirror: String,
    /// 上次加载成功的模型 id（相对模型目录，`/` 分隔）；应用启动时据此后台自动恢复。
    /// 由加载 / 释放命令维护，界面偏好保存不会覆盖它。
    pub last_asr_model: Option<String>,
    /// 上次确认的人声分离模型 id；每次生成字幕前仍会再次让用户确认。
    pub separation_model: Option<String>,

    // ---- 桌面歌词 / 字幕 ----
    /// 桌面歌词开关；模型可用时随启动恢复。由歌词命令维护，整包保存不会覆盖。
    pub desktop_lyrics_enabled: bool,
    /// 字幕预览偏好格式：`srt` 或 `lrc`。
    pub subtitle_format: String,

    // ---- 平台特有（仅对应平台读取）----
    /// Windows：系统媒体传输控制（SMTC）。
    pub smtc: bool,
    /// Windows：系统空间音效。
    pub spatial_audio: bool,
    /// Windows：系统音频增强。
    pub audio_enhancement: bool,
    /// Linux：MPRIS2（仅 Wayland 会话生效）。
    pub mpris2: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            version: PREFS_VERSION,
            library_dir: None,
            favorites: Vec::new(),
            volume: DEFAULT_VOLUME,
            muted: false,
            playback_rate: 1.0,
            play_mode: PlayMode::Sequence,
            sort_by: SortBy::Default,
            crossfade_seconds: DEFAULT_CROSSFADE_SECONDS,
            output_routes: vec![OutputRoute::default()],
            loudness_normalization: true,
            export_scene: String::new(),
            wallpaper: DEFAULT_WALLPAPER.to_owned(),
            custom_wallpaper: None,
            wallpaper_opacity: DEFAULT_WALLPAPER_OPACITY,
            wallpaper_blur: DEFAULT_WALLPAPER_BLUR,
            wallpaper_fit: DEFAULT_WALLPAPER_FIT.to_owned(),
            ui_font: DEFAULT_UI_FONT.to_owned(),
            autostart: false,
            minimize_to_tray: false,
            gpu_acceleration: true,
            autoplay_on_start: false,
            prevent_sleep: false,
            close_action: CloseAction::Ask,
            models_dir: None,
            asr_download_url: DEFAULT_ASR_DOWNLOAD_URL.to_owned(),
            tts_download_urls: BTreeMap::new(),
            cuda_pypi_mirror: DEFAULT_CUDA_PYPI_MIRROR.to_owned(),
            cuda_pytorch_mirror: DEFAULT_CUDA_PYTORCH_MIRROR.to_owned(),
            last_asr_model: None,
            separation_model: None,
            desktop_lyrics_enabled: false,
            subtitle_format: "srt".to_owned(),
            smtc: false,
            spatial_audio: false,
            audio_enhancement: true,
            mpris2: false,
        }
    }
}

impl Prefs {
    /// 把任意输入收敛到可持久化的合法状态。
    pub fn normalize(self) -> Self {
        let rates = PLAYBACK_RATES;
        self.normalize_with_rates(&rates)
    }

    /// 按配置中的倍速档位校验：只允许界面能循环到的倍速。
    pub fn normalize_with_rates(mut self, rates: &[f32]) -> Self {
        self.version = PREFS_VERSION;
        self.library_dir = self
            .library_dir
            .map(|dir| dir.trim().to_owned())
            .filter(|dir| !dir.is_empty());
        self.separation_model = self
            .separation_model
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.is_empty() && id.chars().count() <= MAX_SEPARATION_MODEL_LEN);

        let mut seen = HashSet::new();
        let favorites = self
            .favorites
            .iter()
            .map(|key| key.trim())
            .filter(|key| !key.is_empty() && key.len() <= MAX_KEY_LEN)
            .filter(|key| seen.insert((*key).to_owned()))
            .take(MAX_FAVORITES)
            .map(str::to_owned)
            .collect();
        self.favorites = favorites;

        self.volume = if self.volume.is_finite() {
            self.volume.clamp(0.0, 1.0)
        } else {
            DEFAULT_VOLUME
        };
        let allowed = |rate: f32| {
            rates
                .iter()
                .any(|value| (value - rate).abs() < f32::EPSILON)
        };
        if !allowed(self.playback_rate) {
            self.playback_rate = if allowed(1.0) {
                1.0
            } else {
                rates.first().copied().unwrap_or(1.0)
            };
        }

        let scene = self.export_scene.trim().to_ascii_lowercase();
        self.export_scene = if find_preset(&scene).is_some() {
            scene
        } else {
            // 未选择或未知场景保持为空：归一化仍按默认人耳响度目标执行。
            String::new()
        };

        self.crossfade_seconds = if self.crossfade_seconds.is_finite() {
            self.crossfade_seconds.clamp(0.0, MAX_CROSSFADE_SECONDS)
        } else {
            DEFAULT_CROSSFADE_SECONDS
        };

        // 输出路由按 deviceId 去重，并保证「系统默认输出」始终有一个稳定的入口：
        // 列表为空时默认启用，已有其它设备时默认关闭，避免无意中从扬声器外放。
        let mut seen_devices = HashSet::new();
        let mut routes: Vec<OutputRoute> = std::mem::take(&mut self.output_routes)
            .into_iter()
            .map(OutputRoute::normalize)
            .filter(|route| {
                !route.device_id.is_empty() && seen_devices.insert(route.device_id.clone())
            })
            .take(MAX_OUTPUT_ROUTES)
            .collect();
        if !routes.iter().any(|route| route.device_id == "default") {
            let enabled = routes.is_empty();
            routes.insert(
                0,
                OutputRoute {
                    enabled,
                    ..OutputRoute::default()
                },
            );
        }
        self.output_routes = routes;

        // 自定义壁纸路径只保留带受支持扩展名的值；文件是否还在由运行时判断。
        self.custom_wallpaper = self
            .custom_wallpaper
            .map(|path| path.trim().to_owned())
            .filter(|path| is_wallpaper_path(path));

        let wallpaper = self.wallpaper.trim().to_ascii_lowercase();
        let is_custom = wallpaper == "custom" && self.custom_wallpaper.is_some();
        self.wallpaper = if is_custom || is_builtin_wallpaper_id(&wallpaper) {
            wallpaper
        } else {
            DEFAULT_WALLPAPER.to_owned()
        };

        self.wallpaper_opacity = if self.wallpaper_opacity.is_finite() {
            self.wallpaper_opacity.clamp(0.0, 1.0)
        } else {
            DEFAULT_WALLPAPER_OPACITY
        };

        self.wallpaper_blur = self.wallpaper_blur.min(20);

        self.wallpaper_fit = match self.wallpaper_fit.trim().to_ascii_lowercase().as_str() {
            "contain" => "contain".to_owned(),
            _ => DEFAULT_WALLPAPER_FIT.to_owned(),
        };

        let font = self.ui_font.trim();
        self.ui_font = if font.is_empty()
            || font.chars().count() > MAX_UI_FONT_LEN
            || font
                .chars()
                .any(|c| c.is_control() || matches!(c, '"' | '\\' | ';' | '{' | '}'))
        {
            DEFAULT_UI_FONT.to_owned()
        } else {
            font.to_owned()
        };

        // 模型目录只保留非空路径；是否可用（存在、可写）由运行时判断，
        // 这样目录被临时移走时不会把用户的选择直接丢掉。
        // Windows 的 verbatim 前缀（`\\?\`）会与扫描路径不一致，落盘前统一去掉。
        self.models_dir = self
            .models_dir
            .map(|dir| strip_verbatim_prefix(dir.trim()))
            .filter(|dir| !dir.is_empty() && dir.chars().count() <= MAX_MODELS_DIR_LEN);

        // 下载地址留空或非法时回到内置的 0.6B-int8 默认地址，
        // 保证落盘的永远是一个可用地址，界面与下载逻辑无需各自兜底。
        let download_url = self.asr_download_url.trim();
        self.asr_download_url = if is_download_url(download_url) {
            download_url.to_owned()
        } else {
            DEFAULT_ASR_DOWNLOAD_URL.to_owned()
        };
        self.tts_download_urls = self
            .tts_download_urls
            .into_iter()
            .filter_map(|(id, url)| {
                let id = id.trim();
                if id.is_empty()
                    || id.chars().count() > MAX_TTS_MODEL_ID_LEN
                    || !id.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                    })
                {
                    return None;
                }
                let url = url.trim().trim_end_matches('/');
                if url.is_empty() || !is_download_url(url) || url.contains(['?', '#']) {
                    return None;
                }
                Some((id.to_owned(), url.to_owned()))
            })
            .take(MAX_TTS_DOWNLOAD_SOURCES)
            .collect();

        // CUDA 镜像源同样只允许 http(s) 地址，非法值回退内置默认，保证安装命令始终可用。
        let pypi_mirror = self.cuda_pypi_mirror.trim();
        self.cuda_pypi_mirror = if is_download_url(pypi_mirror) {
            pypi_mirror.to_owned()
        } else {
            DEFAULT_CUDA_PYPI_MIRROR.to_owned()
        };
        let pytorch_mirror = self.cuda_pytorch_mirror.trim();
        self.cuda_pytorch_mirror = if is_download_url(pytorch_mirror) {
            pytorch_mirror.to_owned()
        } else {
            DEFAULT_CUDA_PYTORCH_MIRROR.to_owned()
        };

        // 自动恢复目标必须是模型根目录内的相对 id：绝对路径与 `..` 一律丢弃，
        // 避免手改配置后启动时去加载模型目录以外的文件。
        self.last_asr_model = self
            .last_asr_model
            .map(|id| id.trim().to_owned())
            .filter(|id| {
                !id.is_empty()
                    && id.chars().count() <= MAX_MODELS_DIR_LEN
                    && !id.starts_with('/')
                    && !id.starts_with('\\')
                    // Windows 盘符路径会在 join 时替换掉模型根目录。
                    && !id.contains(':')
                    && !id.split(['/', '\\']).any(|part| part == "..")
            });

        self.subtitle_format = match self.subtitle_format.trim().to_ascii_lowercase().as_str() {
            "lrc" => "lrc".to_owned(),
            _ => "srt".to_owned(),
        };
        self
    }

    /// 读取偏好；文件缺失返回默认值，损坏时返回默认值并附带一条告警。
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let rates = PLAYBACK_RATES;
        Self::load_with_rates(path, &rates)
    }

    /// 按配置中的倍速档位读取偏好。
    pub fn load_with_rates(path: &Path, rates: &[f32]) -> (Self, Option<String>) {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return (Self::default(), None);
            }
            Err(error) => {
                return (
                    Self::default(),
                    Some(format!("读取偏好设置失败，已使用默认值：{error}")),
                );
            }
        };

        match serde_json::from_slice::<Self>(&bytes) {
            Ok(prefs) => {
                let stored_version = prefs.version;
                let mut prefs = prefs.normalize_with_rates(rates);
                let warning = if stored_version < 2 {
                    // v1 的场景预设曾默认勾选；升级后默认不套用场景，
                    // 归一化开关保持用户原值（v1 默认也是开启）。
                    prefs.export_scene = String::new();
                    match prefs.save(path) {
                        Ok(()) => None,
                        Err(error) => Some(format!("偏好设置已升级，但保存迁移结果失败：{error}")),
                    }
                } else {
                    None
                };
                (prefs, warning)
            }
            Err(error) => (
                Self::default(),
                Some(format!("偏好设置文件损坏，已重置为默认值：{error}")),
            ),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), PrefsError> {
        write_json(path, self)?;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum PrefsError {
    #[error(transparent)]
    Persist(#[from] PersistError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let (prefs, warning) = Prefs::load(&dir.path().join("prefs.json"));
        assert_eq!(prefs, Prefs::default());
        assert!(warning.is_none());
    }

    #[test]
    fn corrupt_file_yields_defaults_with_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        fs::write(&path, b"{ not json").unwrap();

        let (prefs, warning) = Prefs::load(&path);
        assert_eq!(prefs, Prefs::default());
        assert!(warning.is_some_and(|w| w.contains("损坏")));
    }

    #[test]
    fn normalize_clamps_and_dedupes() {
        let prefs = Prefs {
            volume: 4.2,
            playback_rate: 3.0,
            favorites: vec![
                " cheerful/a ".into(),
                "cheerful/a".into(),
                "".into(),
                "epic/b".into(),
            ],
            library_dir: Some("  /music  ".into()),
            ..Prefs::default()
        }
        .normalize();

        assert_eq!(prefs.volume, 1.0);
        assert_eq!(prefs.playback_rate, 1.0);
        assert_eq!(prefs.favorites, ["cheerful/a", "epic/b"]);
        assert_eq!(prefs.library_dir.as_deref(), Some("/music"));
    }

    #[test]
    fn export_defaults_to_normalization_on_without_scene() {
        let prefs = Prefs::default();
        assert!(prefs.loudness_normalization);
        assert!(prefs.export_scene.is_empty());
    }

    #[test]
    fn export_scene_is_normalized_and_validated() {
        let normalized = Prefs {
            export_scene: "  PODCAST ".into(),
            loudness_normalization: true,
            ..Prefs::default()
        }
        .normalize();
        assert_eq!(normalized.export_scene, "podcast");
        assert!(normalized.loudness_normalization);

        let unknown = Prefs {
            export_scene: "unknown-scene".into(),
            loudness_normalization: true,
            ..Prefs::default()
        }
        .normalize();
        assert!(unknown.export_scene.is_empty());
        // 未知场景回退为“不套用场景”，但人耳响度归一化仍然开启。
        assert!(unknown.loudness_normalization);
    }

    #[test]
    fn empty_scene_keeps_base_normalization() {
        let prefs = Prefs {
            export_scene: String::new(),
            loudness_normalization: true,
            ..Prefs::default()
        }
        .normalize();
        assert!(prefs.loudness_normalization);
        assert!(prefs.export_scene.is_empty());
    }

    #[test]
    fn version_one_migrates_to_unchecked_scene_with_normalization_on() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        fs::write(
            &path,
            br#"{
                "version": 1,
                "loudnessNormalization": true,
                "exportScene": "video-editing"
            }"#,
        )
        .unwrap();

        let (prefs, warning) = Prefs::load(&path);

        assert!(warning.is_none());
        assert_eq!(prefs.version, PREFS_VERSION);
        assert!(prefs.loudness_normalization);
        assert!(prefs.export_scene.is_empty());

        // 迁移结果应立即落盘，避免每次启动都重新迁移。
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["version"], PREFS_VERSION);
        assert_eq!(saved["loudnessNormalization"], true);
        assert_eq!(saved["exportScene"], "");
    }

    #[test]
    fn crossfade_is_clamped_and_defaulted() {
        let normal = Prefs {
            crossfade_seconds: 30.0,
            ..Prefs::default()
        }
        .normalize();
        assert_eq!(normal.crossfade_seconds, MAX_CROSSFADE_SECONDS);

        let off = Prefs {
            crossfade_seconds: -3.0,
            ..Prefs::default()
        }
        .normalize();
        assert_eq!(off.crossfade_seconds, 0.0);

        let broken = Prefs {
            crossfade_seconds: f32::NAN,
            ..Prefs::default()
        }
        .normalize();
        assert_eq!(broken.crossfade_seconds, DEFAULT_CROSSFADE_SECONDS);
    }

    #[test]
    fn output_routes_are_deduped_and_clamped() {
        let prefs = Prefs {
            output_routes: vec![
                OutputRoute {
                    device_id: " speakers ".into(),
                    label: "".into(),
                    volume: 4.0,
                    muted: false,
                    enabled: true,
                },
                OutputRoute {
                    device_id: "speakers".into(),
                    label: "重复项".into(),
                    volume: 0.2,
                    muted: true,
                    enabled: true,
                },
                OutputRoute {
                    device_id: "".into(),
                    ..OutputRoute::default()
                },
            ],
            ..Prefs::default()
        }
        .normalize();

        assert_eq!(prefs.output_routes.len(), 2);
        // 系统默认输出固定排在第一行，便于界面上快速回到默认路由。
        assert_eq!(prefs.output_routes[0].device_id, "default");
        assert!(!prefs.output_routes[0].enabled);
        assert_eq!(prefs.output_routes[1].device_id, "speakers");
        assert_eq!(prefs.output_routes[1].label, "输出设备");
        assert_eq!(prefs.output_routes[1].volume, 1.0);
    }

    #[test]
    fn empty_output_routes_keep_default_enabled() {
        let prefs = Prefs {
            output_routes: Vec::new(),
            ..Prefs::default()
        }
        .normalize();

        assert_eq!(prefs.output_routes.len(), 1);
        assert_eq!(prefs.output_routes[0].device_id, "default");
        assert!(prefs.output_routes[0].enabled);
    }

    #[test]
    fn roundtrip_preserves_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/prefs.json");
        let prefs = Prefs {
            favorites: vec!["epic/rise".into()],
            volume: 0.4,
            muted: true,
            playback_rate: 1.5,
            play_mode: PlayMode::Shuffle,
            sort_by: SortBy::Name,
            loudness_normalization: false,
            export_scene: "focus".into(),
            library_dir: Some("/tmp/bgm".into()),
            ..Prefs::default()
        };

        prefs.save(&path).unwrap();
        let (loaded, warning) = Prefs::load(&path);

        assert!(warning.is_none());
        assert_eq!(loaded, prefs);
        assert!(!dir.path().join("nested/prefs.json.tmp").exists());
    }

    #[test]
    fn unknown_json_fields_are_ignored() {
        let prefs: Prefs = serde_json::from_str(r#"{ "volume": 0.5, "future": true }"#).unwrap();
        assert_eq!(prefs.volume, 0.5);
        assert_eq!(prefs.play_mode, PlayMode::Sequence);
    }

    #[test]
    fn configured_rates_are_accepted() {
        let rates = [1.0, 1.75, 2.5];
        let prefs = Prefs {
            playback_rate: 1.75,
            ..Prefs::default()
        }
        .normalize_with_rates(&rates);
        assert_eq!(prefs.playback_rate, 1.75);

        let fallback = Prefs {
            playback_rate: 3.5,
            ..Prefs::default()
        }
        .normalize_with_rates(&rates);
        assert_eq!(fallback.playback_rate, 1.0);
    }

    #[test]
    fn unknown_wallpaper_falls_back_to_default() {
        let prefs = Prefs {
            wallpaper: "  GRADIENT-BLUE ".into(),
            ..Prefs::default()
        }
        .normalize();
        assert_eq!(prefs.wallpaper, "gradient-blue");

        let broken = Prefs {
            wallpaper: "gradient-purple".into(),
            ..Prefs::default()
        }
        .normalize();
        assert_eq!(broken.wallpaper, DEFAULT_WALLPAPER);
    }

    #[test]
    fn discovered_builtin_wallpaper_ids_are_kept() {
        // 自动发现的壁纸 id 由文件名生成，归一化不能再按固定清单拦截。
        for id in ["builtin-1", "builtin-3", "builtin-night_view"] {
            let prefs = Prefs {
                wallpaper: id.into(),
                ..Prefs::default()
            }
            .normalize();
            assert_eq!(prefs.wallpaper, id);
        }

        for bad in ["builtin-", "builtin-../x", "builtin-夜"] {
            let prefs = Prefs {
                wallpaper: bad.into(),
                ..Prefs::default()
            }
            .normalize();
            assert_eq!(prefs.wallpaper, DEFAULT_WALLPAPER, "{bad} 不应保留");
        }
    }

    #[test]
    fn custom_wallpaper_requires_supported_file() {
        let missing = Prefs {
            wallpaper: "custom".into(),
            ..Prefs::default()
        }
        .normalize();
        assert_eq!(missing.wallpaper, "default");
        assert!(missing.custom_wallpaper.is_none());

        let unsupported = Prefs {
            wallpaper: "custom".into(),
            custom_wallpaper: Some("C:/壁纸/动画.gif".into()),
            ..Prefs::default()
        }
        .normalize();
        assert_eq!(unsupported.wallpaper, "default");
        assert!(unsupported.custom_wallpaper.is_none());

        let valid = Prefs {
            wallpaper: "custom".into(),
            custom_wallpaper: Some("  C:/壁纸/夜空.MP4 ".into()),
            ..Prefs::default()
        }
        .normalize();
        assert_eq!(valid.wallpaper, "custom");
        assert_eq!(valid.custom_wallpaper.as_deref(), Some("C:/壁纸/夜空.MP4"));
    }

    #[test]
    fn custom_path_is_kept_after_switching_away() {
        let prefs = Prefs {
            wallpaper: "gradient-green".into(),
            custom_wallpaper: Some("/pics/clouds.webp".into()),
            ..Prefs::default()
        }
        .normalize();
        assert_eq!(prefs.wallpaper, "gradient-green");
        assert_eq!(prefs.custom_wallpaper.as_deref(), Some("/pics/clouds.webp"));
    }

    #[test]
    fn wallpaper_opacity_clamped_and_defaulted() {
        let prefs = Prefs {
            wallpaper_opacity: 1.5,
            ..Prefs::default()
        }
        .normalize();
        assert_eq!(prefs.wallpaper_opacity, 1.0);

        let prefs_neg = Prefs {
            wallpaper_opacity: -0.2,
            ..Prefs::default()
        }
        .normalize();
        assert_eq!(prefs_neg.wallpaper_opacity, 0.0);

        let prefs_nan = Prefs {
            wallpaper_opacity: f32::NAN,
            ..Prefs::default()
        }
        .normalize();
        assert_eq!(prefs_nan.wallpaper_opacity, DEFAULT_WALLPAPER_OPACITY);
    }

    #[test]
    fn wallpaper_blur_and_fit_normalized() {
        let prefs = Prefs {
            wallpaper_blur: 50,
            wallpaper_fit: "  CONTAIN  ".into(),
            ..Prefs::default()
        }
        .normalize();
        assert_eq!(prefs.wallpaper_blur, 20);
        assert_eq!(prefs.wallpaper_fit, "contain");

        let invalid_fit = Prefs {
            wallpaper_fit: "stretch".into(),
            ..Prefs::default()
        }
        .normalize();
        assert_eq!(invalid_fit.wallpaper_fit, "cover");
    }

    #[test]
    fn system_settings_defaults_match_spec() {
        let prefs = Prefs::default();
        assert_eq!(prefs.ui_font, DEFAULT_UI_FONT);
        assert!(!prefs.autostart);
        assert!(!prefs.minimize_to_tray);
        assert!(prefs.gpu_acceleration);
        assert!(!prefs.autoplay_on_start);
        assert!(!prefs.prevent_sleep);
        assert_eq!(prefs.close_action, CloseAction::Ask);
        // Windows 平台特有项：SMTC / 空间音效默认关闭，音频增强默认开启。
        assert!(!prefs.smtc);
        assert!(!prefs.spatial_audio);
        assert!(prefs.audio_enhancement);
        // Linux 平台特有项：MPRIS2 默认关闭。
        assert!(!prefs.mpris2);
    }

    #[test]
    fn model_settings_default_to_exe_models_dir_with_builtin_url() {
        let prefs = Prefs::default();
        // `None` 表示系统「下载」目录下的 bgm-models/；下载地址默认显式指向 0.6B-int8 目录。
        assert!(prefs.models_dir.is_none());
        assert_eq!(
            prefs.asr_download_url,
            "https://modelscope.cn/models/zengshuishui/Qwen3-ASR-onnx/tree/master/model_0.6B"
        );
        assert_eq!(prefs.asr_download_url, DEFAULT_ASR_DOWNLOAD_URL);
    }

    #[test]
    fn model_settings_normalize_paths_and_urls() {
        let prefs = Prefs {
            models_dir: Some("  D:/听风吟/models  ".into()),
            asr_download_url:
                " https://modelscope.cn/models/zengshuishui/Qwen3-ASR-onnx/tree/master/model_0.6B "
                    .into(),
            ..Prefs::default()
        }
        .normalize();
        assert_eq!(prefs.models_dir.as_deref(), Some("D:/听风吟/models"));
        assert!(prefs.asr_download_url.starts_with("https://modelscope.cn/"));

        let invalid = Prefs {
            models_dir: Some("   ".into()),
            asr_download_url: "file:///models".into(),
            ..Prefs::default()
        }
        .normalize();
        assert!(invalid.models_dir.is_none());
        // 非法地址回退到内置默认地址，而不是留下一个无法请求的链接。
        assert_eq!(invalid.asr_download_url, DEFAULT_ASR_DOWNLOAD_URL);
        let mut sources = BTreeMap::new();
        sources.insert(
            "zipvoice-int8".into(),
            " https://mirror.example.com/tts/ ".into(),
        );
        sources.insert("../bad".into(), "https://mirror.example.com/x".into());
        sources.insert("zipvoice-fp32".into(), "file:///tmp".into());
        sources.insert(
            TTS_COSYVOICE_MODEL_ID.into(),
            "https://www.modelscope.cn/models/owner/repo/files?x=1".into(),
        );
        let normalized = Prefs {
            tts_download_urls: sources,
            ..Prefs::default()
        }
        .normalize();
        assert_eq!(normalized.tts_download_urls.len(), 1);
        assert_eq!(
            normalized
                .tts_download_urls
                .get("zipvoice-int8")
                .map(String::as_str),
            Some("https://mirror.example.com/tts")
        );

        // Windows canonicalize 的 verbatim 前缀不能写进配置，否则与扫描路径不相等。
        let verbatim = Prefs {
            models_dir: Some(r"\\?\D:\听风吟\bgm-models".into()),
            ..Prefs::default()
        }
        .normalize();
        assert_eq!(
            verbatim.models_dir.as_deref(),
            Some(r"D:\听风吟\bgm-models")
        );

        let unc = Prefs {
            models_dir: Some(r"\\?\UNC\nas\models".into()),
            ..Prefs::default()
        }
        .normalize();
        assert_eq!(unc.models_dir.as_deref(), Some(r"\\nas\models"));
    }

    #[test]
    fn last_asr_model_accepts_relative_ids_only() {
        let relative = Prefs {
            last_asr_model: Some("  sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25  ".into()),
            ..Prefs::default()
        }
        .normalize();
        assert_eq!(
            relative.last_asr_model.as_deref(),
            Some("sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25")
        );

        // 绝对路径与越界路径不能成为启动自动加载目标。
        for unsafe_id in ["/models/x", r"D:\models\x", "a/../b", "  "] {
            let prefs = Prefs {
                last_asr_model: Some(unsafe_id.into()),
                ..Prefs::default()
            }
            .normalize();
            assert!(prefs.last_asr_model.is_none(), "{unsafe_id} 不应保留");
        }
    }

    #[test]
    fn lyrics_and_subtitle_defaults_normalize() {
        let prefs = Prefs::default();
        assert!(!prefs.desktop_lyrics_enabled);
        assert_eq!(prefs.subtitle_format, "srt");

        // 偏好格式只保留 srt / lrc，其它值回退 srt。
        for (input, expected) in [(" LRC ", "lrc"), ("srt", "srt"), ("vtt", "srt")] {
            let normalized = Prefs {
                subtitle_format: input.into(),
                ..Prefs::default()
            }
            .normalize();
            assert_eq!(normalized.subtitle_format, expected);
        }
    }

    #[test]
    fn lyrics_and_subtitle_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        let prefs = Prefs {
            desktop_lyrics_enabled: true,
            subtitle_format: "lrc".into(),
            ..Prefs::default()
        };
        prefs.save(&path).unwrap();
        let (loaded, warning) = Prefs::load(&path);
        assert!(warning.is_none());
        assert!(loaded.desktop_lyrics_enabled);
        assert_eq!(loaded.subtitle_format, "lrc");
    }

    #[test]
    fn missing_system_fields_fall_back_to_defaults() {
        // 旧版本 prefs.json 没有这些字段：容器级 serde(default) 必须取结构体默认值，
        // 而不是 bool 的零值，否则 GPU 加速与音频增强会被错误地默认关闭。
        let prefs: Prefs = serde_json::from_str(r#"{ "volume": 0.5 }"#).unwrap();
        assert_eq!(prefs.volume, 0.5);
        assert!(prefs.gpu_acceleration);
        assert!(prefs.audio_enhancement);
        assert!(!prefs.smtc);
        assert!(!prefs.spatial_audio);
        assert!(!prefs.mpris2);
        assert_eq!(prefs.ui_font, DEFAULT_UI_FONT);
    }

    #[test]
    fn ui_font_accepts_system_family_names() {
        // 字体选择改为读取系统字体后，族名（含空格、中文）都应原样保留。
        let family = Prefs {
            ui_font: "  Microsoft YaHei  ".into(),
            ..Prefs::default()
        }
        .normalize();
        assert_eq!(family.ui_font, "Microsoft YaHei");

        let chinese = Prefs {
            ui_font: "站酷高端黑".into(),
            ..Prefs::default()
        }
        .normalize();
        assert_eq!(chinese.ui_font, "站酷高端黑");

        let empty = Prefs {
            ui_font: "   ".into(),
            ..Prefs::default()
        }
        .normalize();
        assert_eq!(empty.ui_font, DEFAULT_UI_FONT);

        // 会破坏根元素 CSS 变量值的字符一律回退默认字体。
        let unsafe_value = Prefs {
            ui_font: "\"Onest\", url(evil)".into(),
            ..Prefs::default()
        }
        .normalize();
        assert_eq!(unsafe_value.ui_font, DEFAULT_UI_FONT);
    }

    #[test]
    fn system_settings_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        let prefs = Prefs {
            ui_font: "yahei".into(),
            autostart: true,
            minimize_to_tray: true,
            gpu_acceleration: false,
            autoplay_on_start: true,
            prevent_sleep: true,
            close_action: CloseAction::Tray,
            smtc: true,
            spatial_audio: true,
            audio_enhancement: false,
            mpris2: true,
            ..Prefs::default()
        };

        prefs.save(&path).unwrap();
        let (loaded, warning) = Prefs::load(&path);
        assert!(warning.is_none());
        assert_eq!(loaded, prefs);
    }
}
