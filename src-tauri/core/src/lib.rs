//! TFY BGM 的设备无关核心逻辑：曲库扫描、写操作、配置持久化与音频导出。
//! 该 crate 不依赖 Tauri，可直接 `cargo test -p bgm-core`。

pub mod categories;
pub mod config;
pub mod export;
pub mod library;
pub mod limits;
pub mod loudness;
pub mod loops;
pub mod ops;
pub mod paths;
pub mod persist;
pub mod prefs;
pub mod schedule;
pub mod tags;

pub use categories::{Categories, Category};
pub use config::AppConfig;
pub use export::{ExportOutcome, ExportSettings};
pub use library::{LibrarySnapshot, LibrarySource, Song, scan_library, scan_library_cached};
pub use loudness::ExportPreset;
pub use loops::{LoopBook, LoopPoint};
pub use ops::{BatchOutcome, BatchReport, KeyChange};
pub use prefs::{PlayMode, Prefs, SortBy};
pub use schedule::{Schedule, ScheduleEntry};
