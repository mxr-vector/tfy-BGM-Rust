//! 按业务领域拆分的 HTTP 处理器；由 [`crate::api::router`] 注册。

mod library;
mod models;
mod playback;
mod separation;
mod subtitles;
mod system;
mod tasks;

pub(crate) use library::{library, rescan};
pub(crate) use models::{models_info, models_load, models_unload};
pub(crate) use playback::{
    command, crossfade, next, outputs, pause, play, previous, resume, seek, toggle, update_output,
    volume,
};
pub(crate) use separation::{separation_create, separation_models, separation_result};
#[cfg(test)]
pub(crate) use subtitles::content_disposition;
pub(crate) use subtitles::{
    subtitle_delete, subtitle_export, subtitle_get, subtitle_update, subtitle_update_text,
    subtitles_generate, subtitles_list,
};
pub(crate) use system::{events, get_state, health};
pub(crate) use tasks::{task_events, task_get, tasks_list};
