//! TFY BGM 的设备无关核心逻辑：曲库扫描、写操作、配置持久化与音频导出。
//! 该 crate 不依赖 Tauri，可直接 `cargo test -p bgm-core`。

pub mod audio_index;
pub mod categories;
pub mod config;
pub mod export;
pub mod library;
pub mod limits;
pub mod loops;
pub mod loudness;
pub mod ops;
pub mod paths;
pub mod persist;
pub mod prefs;
pub mod process;
pub mod schedule;
pub mod tags;

pub use categories::{Categories, Category};
pub use config::{ApiConfig, AppConfig, CorsConfig, CorsMode};
pub use export::{ExportOutcome, ExportSettings};
pub use library::{LibrarySnapshot, LibrarySource, Song, scan_library, scan_library_cached};
pub use loops::{LoopBook, LoopPoint};
pub use loudness::ExportPreset;
pub use ops::{BatchOutcome, BatchReport, KeyChange};
pub use prefs::{CloseAction, OutputRoute, PlayMode, Prefs, SortBy};
pub use schedule::{Schedule, ScheduleEntry, ScheduleSong};
