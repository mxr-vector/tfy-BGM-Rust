//! 应用级配置：品牌文案、播放器行为参数与「全部 / 收藏」页文案。
//!
//! 与 `prefs.json`（运行时偏好）、`categories.json`（曲库分类）并列存放在应用配置目录，
//! 首次启动按内置默认值生成 `config.json`，之后可直接编辑该文件，
//! 通过界面上的「重新扫描」重新载入。字段缺失或取值越界会逐项回退默认值。

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::persist::{PersistError, write_json};
use crate::prefs::PLAYBACK_RATES;

const MAX_RATES: usize = 10;
const MIN_RATE: f32 = 0.25;
const MAX_RATE: f32 = 4.0;
const MIN_VOLUME_STEP: f32 = 0.01;
const MAX_VOLUME_STEP: f32 = 0.5;
const MIN_SEEK_STEP: f64 = 1.0;
const MAX_SEEK_STEP: f64 = 60.0;
const MIN_RESTART_SECONDS: f64 = 1.0;
const MAX_RESTART_SECONDS: f64 = 30.0;
const MIN_AUTO_SKIPS: u32 = 0;
const MAX_AUTO_SKIPS: u32 = 20;
const MIN_TIMEOUT_MS: u64 = 1_000;
const MAX_TIMEOUT_MS: u64 = 120_000;

/// `config.json` 的完整结构。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppConfig {
    pub brand: BrandInfo,
    pub player: PlayerConfig,
    pub ui: UiCopy,
}

/// 品牌信息：界面标题、副标题与系统窗口标题。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BrandInfo {
    pub title: String,
    pub subtitle: String,
    pub window_title: String,
}

impl Default for BrandInfo {
    fn default() -> Self {
        Self {
            title: "听风吟 · BGM".to_owned(),
            subtitle: "剪辑音频素材库".to_owned(),
            window_title: "听风吟 · TFY BGM".to_owned(),
        }
    }
}

/// 播放器行为参数：倍速档位、步进与失败跳歌上限。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PlayerConfig {
    /// 倍速循环顺序，界面按钮按该顺序轮换。
    pub rates: Vec<f32>,
    /// 音量快捷键与滑杆的步进。
    pub volume_step: f32,
    /// 左右方向键与进度条的跳转秒数。
    pub seek_step_seconds: f64,
    /// 「上一首」在当前曲目播放超过该秒数后先回到开头。
    pub restart_threshold_seconds: f64,
    /// 音频连续加载失败时最多自动跳过几首，`0` 表示不自动跳过。
    pub max_auto_skips: u32,
    /// 读取音频 Blob 的超时时间（毫秒）。
    pub load_timeout_ms: u64,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self {
            rates: PLAYBACK_RATES.to_vec(),
            volume_step: 0.1,
            seek_step_seconds: 5.0,
            restart_threshold_seconds: 3.0,
            max_auto_skips: 3,
            load_timeout_ms: 20_000,
        }
    }
}

/// 「全部」与「收藏」两个虚拟分类的展示文案。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiCopy {
    pub all: HeroCopy,
    pub favorites: HeroCopy,
}

/// 页面主视觉文案；`description` 支持 `{categories}` 占位符。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct HeroCopy {
    pub title: String,
    pub label_en: String,
    pub description: String,
    pub results_title: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            brand: BrandInfo::default(),
            player: PlayerConfig::default(),
            ui: UiCopy {
                all: HeroCopy {
                    title: "全部精选视频剪辑 BGM".to_owned(),
                    label_en: "ALL BGM SOUNDTRACKS".to_owned(),
                    description: "汇集{categories}等分类的优质背景音乐，覆盖 Vlog、宣传片、故事片、卡点等全场景剪辑需求。".to_owned(),
                    results_title: "全部音乐素材".to_owned(),
                },
                favorites: HeroCopy {
                    title: "我的收藏夹 · Favorite Tracks".to_owned(),
                    label_en: "FAVORITES".to_owned(),
                    description: "你标记喜欢的配乐都在这里，方便在剪辑过程中随时试听与导出使用。".to_owned(),
                    results_title: "我的收藏夹".to_owned(),
                },
            },
        }
    }
}

impl AppConfig {
    /// 读取配置；文件缺失返回内置默认值，损坏时返回默认值并附带一条告警。
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return (Self::default(), None);
            }
            Err(error) => {
                return (
                    Self::default(),
                    Some(format!("读取应用配置失败，已使用默认配置：{error}")),
                );
            }
        };

        match serde_json::from_slice::<Self>(&bytes) {
            Ok(parsed) => {
                let config = parsed.clone().normalize();
                let warning = (config != parsed)
                    .then(|| "应用配置存在无效或缺失项，已自动修正为可用取值".to_owned());
                (config, warning)
            }
            Err(error) => (
                Self::default(),
                Some(format!("应用配置文件损坏，已重置为默认配置：{error}")),
            ),
        }
    }

    /// 首次启动时把默认配置写入文件，方便用户直接用编辑器维护。
    pub fn ensure_file(&self, path: &Path) -> Result<(), PersistError> {
        if path.exists() {
            return Ok(());
        }
        self.save(path)
    }

    pub fn save(&self, path: &Path) -> Result<(), PersistError> {
        write_json(path, self)
    }

    /// 把任意输入收敛到可用取值：空文案回退默认、数值 clamp 到安全区间。
    pub fn normalize(mut self) -> Self {
        let defaults = Self::default();

        let brand = std::mem::take(&mut self.brand);
        self.brand = brand.normalize(&defaults.brand);

        let player = std::mem::take(&mut self.player);
        self.player = player.normalize(&defaults.player);

        let all = std::mem::take(&mut self.ui.all);
        self.ui.all = all.normalize(&defaults.ui.all);
        let favorites = std::mem::take(&mut self.ui.favorites);
        self.ui.favorites = favorites.normalize(&defaults.ui.favorites);
        self
    }
}

impl BrandInfo {
    fn normalize(mut self, fallback: &Self) -> Self {
        self.title = text_or(&self.title, &fallback.title);
        self.subtitle = text_or(&self.subtitle, &fallback.subtitle);
        self.window_title = text_or(&self.window_title, &fallback.window_title);
        self
    }
}

impl PlayerConfig {
    fn normalize(mut self, fallback: &Self) -> Self {
        self.rates = sanitize_rates(self.rates, &fallback.rates);
        self.volume_step = ratio32(
            self.volume_step,
            fallback.volume_step,
            MIN_VOLUME_STEP,
            MAX_VOLUME_STEP,
        );
        self.seek_step_seconds = ratio64(
            self.seek_step_seconds,
            fallback.seek_step_seconds,
            MIN_SEEK_STEP,
            MAX_SEEK_STEP,
        );
        self.restart_threshold_seconds = ratio64(
            self.restart_threshold_seconds,
            fallback.restart_threshold_seconds,
            MIN_RESTART_SECONDS,
            MAX_RESTART_SECONDS,
        );
        self.max_auto_skips = self.max_auto_skips.clamp(MIN_AUTO_SKIPS, MAX_AUTO_SKIPS);
        self.load_timeout_ms = self.load_timeout_ms.clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS);
        self
    }
}

impl HeroCopy {
    fn normalize(mut self, fallback: &Self) -> Self {
        self.title = text_or(&self.title, &fallback.title);
        self.label_en = text_or(&self.label_en, &fallback.label_en);
        self.description = text_or(&self.description, &fallback.description);
        self.results_title = text_or(&self.results_title, &fallback.results_title);
        self
    }
}

fn text_or(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// 非正数或 NaN 视为「未配置」并使用默认值，其余夹到安全区间。
fn ratio32(value: f32, fallback: f32, min: f32, max: f32) -> f32 {
    if value.is_finite() && value > 0.0 {
        value.clamp(min, max)
    } else {
        fallback
    }
}

fn ratio64(value: f64, fallback: f64, min: f64, max: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value.clamp(min, max)
    } else {
        fallback
    }
}

fn sanitize_rates(rates: Vec<f32>, fallback: &[f32]) -> Vec<f32> {
    let mut cleaned: Vec<f32> = Vec::new();
    for rate in rates {
        if !rate.is_finite() {
            continue;
        }
        let rate = rate.clamp(MIN_RATE, MAX_RATE);
        if cleaned.iter().any(|seen| (seen - rate).abs() < 0.001) {
            continue;
        }
        cleaned.push(rate);
        if cleaned.len() == MAX_RATES {
            break;
        }
    }
    if cleaned.is_empty() {
        fallback.to_vec()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn defaults_cover_brand_and_player() {
        let config = AppConfig::default();
        assert_eq!(config.brand.title, "听风吟 · BGM");
        assert_eq!(config.player.rates, PLAYBACK_RATES.to_vec());
        assert!(config.ui.all.description.contains("{categories}"));
    }

    #[test]
    fn missing_file_yields_defaults_without_warning() {
        let dir = tempfile::tempdir().unwrap();
        let (config, warning) = AppConfig::load(&dir.path().join("config.json"));
        assert_eq!(config, AppConfig::default());
        assert!(warning.is_none());
    }

    #[test]
    fn corrupt_file_yields_defaults_with_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, b"{ not json").unwrap();

        let (config, warning) = AppConfig::load(&path);
        assert_eq!(config, AppConfig::default());
        assert!(warning.is_some_and(|w| w.contains("损坏")));
    }

    #[test]
    fn roundtrip_persists_custom_copy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/config.json");
        let mut config = AppConfig::default();
        config.brand.title = "我的 BGM 库".into();
        config.ui.all.results_title = "全部曲目".into();
        config.player.rates = vec![1.0, 1.5];

        config.ensure_file(&path).unwrap();
        config.ensure_file(&path).unwrap();
        let (loaded, warning) = AppConfig::load(&path);

        assert!(warning.is_none(), "{warning:?}");
        assert_eq!(loaded.brand.title, "我的 BGM 库");
        assert_eq!(loaded.ui.all.results_title, "全部曲目");
        assert_eq!(loaded.player.rates, vec![1.0, 1.5]);
    }

    #[test]
    fn normalize_falls_back_for_empty_text_and_out_of_range_numbers() {
        let raw = r#"{
            "brand": { "title": "  ", "windowTitle": "自定义窗口" },
            "player": { "rates": [], "volumeStep": 9, "seekStepSeconds": 0, "maxAutoSkips": 0, "loadTimeoutMs": 1 },
            "ui": { "all": { "description": "  " } }
        }"#;
        let parsed: AppConfig = serde_json::from_str(raw).unwrap();
        let config = parsed.normalize();

        assert_eq!(config.brand.title, AppConfig::default().brand.title);
        assert_eq!(config.brand.subtitle, AppConfig::default().brand.subtitle);
        assert_eq!(config.brand.window_title, "自定义窗口");
        assert_eq!(config.player.rates, PLAYBACK_RATES.to_vec());
        assert_eq!(config.player.volume_step, MAX_VOLUME_STEP);
        assert_eq!(
            config.player.seek_step_seconds,
            AppConfig::default().player.seek_step_seconds
        );
        assert_eq!(config.player.max_auto_skips, 0);
        assert_eq!(config.player.load_timeout_ms, MIN_TIMEOUT_MS);
        assert_eq!(
            config.ui.all.description,
            AppConfig::default().ui.all.description
        );
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let config: AppConfig =
            serde_json::from_str(r#"{ "brand": { "title": "T" }, "future": true }"#).unwrap();
        assert_eq!(config.brand.title, "T");
    }

    /// 前端按 camelCase 读取，字段名改错只会静默失效，这里锁住序列化契约。
    #[test]
    fn serializes_camel_case_keys_for_frontend() {
        let json = serde_json::to_value(AppConfig::default()).unwrap();
        assert!(json["brand"]["windowTitle"].is_string());
        assert!(json["brand"]["subtitle"].is_string());
        assert!(json["player"]["volumeStep"].is_number());
        assert!(json["player"]["seekStepSeconds"].is_number());
        assert!(json["player"]["restartThresholdSeconds"].is_number());
        assert!(json["player"]["maxAutoSkips"].is_number());
        assert!(json["player"]["loadTimeoutMs"].is_number());
        assert!(json["ui"]["all"]["labelEn"].is_string());
        assert!(json["ui"]["all"]["resultsTitle"].is_string());
        assert!(json["ui"]["favorites"]["resultsTitle"].is_string());
    }
}
