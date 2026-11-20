//! 启动期设置加载：读取 `config.json`、`prefs.json`、`categories.json`。
//!
//! 三个配置文件并列存放在应用配置目录；缺失时写入默认值，损坏或取值异常时
//! 回退到可用状态并收集告警，避免单个文件的问题阻断整个应用启动。

use std::path::PathBuf;

use anyhow::{Context, Result};
use bgm_core::paths::{CATEGORIES_FILE, CONFIG_FILE, LOOPS_FILE, PREFS_FILE, SCHEDULE_FILE};
use bgm_core::{AppConfig, Categories, LoopBook, Prefs, Schedule};
use tauri::{AppHandle, Manager};

pub struct Settings {
    pub config_path: PathBuf,
    pub prefs_path: PathBuf,
    pub categories_path: PathBuf,
    pub schedule_path: PathBuf,
    pub loops_path: PathBuf,
    pub config: AppConfig,
    pub prefs: Prefs,
    pub categories: Categories,
    pub schedule: Schedule,
    pub loops: LoopBook,
    /// 加载过程中产生的可展示告警，按 config → prefs → categories 的顺序排列。
    pub warnings: Vec<String>,
}

impl Settings {
    pub fn load(app: &AppHandle) -> Result<Self> {
        let config_dir = app
            .path()
            .app_config_dir()
            .context("无法定位应用配置目录")?;
        let mut warnings = Vec::new();

        let config_path = config_dir.join(CONFIG_FILE);
        let (config, config_warning) = AppConfig::load(&config_path);
        if let Some(warning) = config_warning {
            warnings.push(warning);
        }
        if let Err(error) = config.ensure_file(&config_path) {
            warnings.push(format!("无法写入应用配置文件：{error}"));
        }

        let prefs_path = config_dir.join(PREFS_FILE);
        let (prefs, prefs_warning) = Prefs::load_with_rates(&prefs_path, &config.player.rates);
        if let Some(warning) = prefs_warning {
            warnings.push(warning);
        }

        let categories_path = config_dir.join(CATEGORIES_FILE);
        let (categories, categories_warning) = Categories::load(&categories_path);
        if let Some(warning) = categories_warning {
            warnings.push(warning);
        }
        if let Err(error) = categories.ensure_file(&categories_path) {
            warnings.push(format!("无法写入分类配置文件：{error}"));
        }

        let schedule_path = config_dir.join(SCHEDULE_FILE);
        let (schedule, schedule_warning) = Schedule::load(&schedule_path);
        if let Some(warning) = schedule_warning {
            warnings.push(warning);
        }

        let loops_path = config_dir.join(LOOPS_FILE);
        let (loops, loops_warning) = LoopBook::load(&loops_path);
        if let Some(warning) = loops_warning {
            warnings.push(warning);
        }

        Ok(Self {
            config_path,
            prefs_path,
            categories_path,
            schedule_path,
            loops_path,
            config,
            prefs,
            categories,
            schedule,
            loops,
            warnings,
        })
    }
}
