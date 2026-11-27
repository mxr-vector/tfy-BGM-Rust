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
/// 节目单（排程）配置。
pub const SCHEDULE_FILE: &str = "schedule.json";
/// 单曲循环区间配置。
pub const LOOPS_FILE: &str = "loops.json";
/// 本地音频索引与指纹缓存。
pub const AUDIO_INDEX_FILE: &str = "audio-index.json";

/// 曲库目录名（内置资源或外置挂载点均使用该名称）。
pub const LIBRARY_DIR_NAME: &str = "bgm";
/// ASR 模型目录名（默认位于系统「下载」目录下，带 `bgm-` 前缀以免与其它模型目录混淆）。
pub const MODELS_DIR_NAME: &str = "bgm-models";
/// 指定曲库目录的环境变量。
pub const LIBRARY_ENV_VAR: &str = "TFY_BGM_DIR";
/// 软删除回收站目录名。
pub const TRASH_DIR_NAME: &str = ".tfy-trash";

/// 去掉 Windows 扩展长度路径前缀（`\\?\` / `\\?\UNC\`），用于持久化与界面展示。
///
/// `fs::canonicalize` 在 Windows 上返回 verbatim 路径；直接写进配置或展示会出现
/// `\\?\D:\...`，并且与扫描得到的普通路径做字符串比较时永远不相等。
pub fn strip_verbatim_prefix(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        path.to_owned()
    }
}
