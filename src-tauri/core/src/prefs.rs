//! 用户偏好：收藏、音量、倍速、播放模式、排序与界面壁纸。
//!
//! 所有来自前端的数据都会经过 `normalize`，落盘采用「临时文件 + 重命名」，
//! 损坏的偏好文件回退为默认值而不是让应用启动失败。

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::limits::{MAX_FAVORITES, MAX_KEY_LEN};
use crate::persist::{PersistError, write_json};

pub const PREFS_VERSION: u32 = 1;
pub const DEFAULT_VOLUME: f32 = 0.85;
pub const PLAYBACK_RATES: [f32; 5] = [0.75, 1.0, 1.25, 1.5, 2.0];

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
/// 内置壁纸选项的合法 id；`custom` 需要搭配自定义文件路径才生效。
pub const BUILTIN_WALLPAPER_IDS: [&str; 5] = [
    "gradient-blue",
    "gradient-green",
    "gradient-red",
    "builtin-1",
    "builtin-2",
];

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
            wallpaper: DEFAULT_WALLPAPER.to_owned(),
            custom_wallpaper: None,
            wallpaper_opacity: DEFAULT_WALLPAPER_OPACITY,
            wallpaper_blur: DEFAULT_WALLPAPER_BLUR,
            wallpaper_fit: DEFAULT_WALLPAPER_FIT.to_owned(),
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

        // 自定义壁纸路径只保留带受支持扩展名的值；文件是否还在由运行时判断。
        self.custom_wallpaper = self
            .custom_wallpaper
            .map(|path| path.trim().to_owned())
            .filter(|path| is_wallpaper_path(path));

        let wallpaper = self.wallpaper.trim().to_ascii_lowercase();
        let is_builtin = BUILTIN_WALLPAPER_IDS.contains(&wallpaper.as_str());
        let is_custom = wallpaper == "custom" && self.custom_wallpaper.is_some();
        self.wallpaper = if is_builtin || is_custom {
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
            Ok(prefs) => (prefs.normalize_with_rates(rates), None),
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
}
