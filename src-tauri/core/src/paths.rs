//! 全局约定名：配置文件、曲库目录与环境变量。
//!
//! 这些名字同时出现在 Rust 逻辑、前端提示和 README 中，集中在这里可以避免
//! 改一处漏一处；需要调整目录结构时只改本文件。

/// 运行时偏好（音量、倍速、收藏、曲库目录）。
pub const PREFS_FILE: &str = "prefs.json";
/// 曲库分类元数据。
pub const CATEGORIES_FILE: &str = "categories.json";
/// 应用级配置（品牌文案、播放器参数、界面文案）。
pub const CONFIG_FILE: &str = "config.json";

/// 外置曲库目录名（默认位于 exe 同级）。
pub const LIBRARY_DIR_NAME: &str = "bgm";
/// 指定曲库目录的环境变量。
pub const LIBRARY_ENV_VAR: &str = "TFY_BGM_DIR";
/// 软删除回收站目录名。
pub const TRASH_DIR_NAME: &str = ".tfy-trash";
