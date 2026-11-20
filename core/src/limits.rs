//! 跨模块共享的长度与数量上限。
//!
//! 这些数值同时用于校验、错误提示与截断逻辑，集中一处避免「改名 40、截断 120」
//! 这类各写一份的偏差。

/// 分类名称 / 目录名的最大字符数。
pub const MAX_NAME_CHARS: usize = 40;
/// 曲名 / 文件名主干的最大字符数。
pub const MAX_STEM_CHARS: usize = 120;
/// 曲目稳定 key（`分类/文件名主干`）的最大字节数。
pub const MAX_KEY_LEN: usize = 512;
/// 收藏曲目的数量上限。
pub const MAX_FAVORITES: usize = 10_000;
