//! TFY BGM 的设备无关核心逻辑：曲库扫描、偏好持久化与音频导出。
//! 该 crate 不依赖 Tauri，可直接 `cargo test -p bgm-core`。

pub mod categories;
pub mod export;
pub mod library;
pub mod prefs;

pub use categories::{CATEGORIES, Category};
pub use library::{LibrarySnapshot, LibrarySource, Song, scan_library};
pub use prefs::{PlayMode, Prefs, SortBy};
