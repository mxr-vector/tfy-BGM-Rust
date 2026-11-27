//! 文生语音工作室：模型管理、内置/录音音色、脚本预览与 WAV 交付。

use std::fs::{self, File};
use std::io::Read;
use std::sync::atomic::Ordering;
use std::sync::mpsc::TryRecvError;

use gpui::ScrollHandle;

use crate::audio::AudioPreview;
use crate::library::tts::{
    self, BuiltinReference, DEFAULT_KOKORO_SPEAKER, Family, Format, KokoroVoice, Model, Output,
    Progress, Reference, Segment, Session, VoiceCategory,
};
use crate::microphone::Recording;

use super::audio_tools::{
    tool_banner, tool_header, tool_header_badge, tool_message, tool_page, tool_panel,
    tool_progress_bar,
};
use super::*;

#[derive(Clone, Copy)]
enum DeliveryAction {
    Edit,
    Install,
    InstallSherpaRuntime,
    PrepareRuntime,
    ChooseVoice,
    Generate,
}

const RECORDING_PROMPT: &str = "心之所向，素履所往，生如逆旅，一苇以航。";

enum Completion {
    ModelFiles,
    RuntimePrepared,
    Generated {
        output: Output,
        session: Arc<Session>,
        revision: u64,
    },
    RecordedReference(tempfile::TempPath),
    BuiltinReference {
        voice: String,
        reference: BuiltinReference,
    },
}

pub(super) struct SpeechTools {
    models_root: PathBuf,
    family: Family,
    model: Model,
    model_check: Result<(), String>,
    model_exists: bool,
    pub(super) sherpa_runtime_ready: bool,
    cosyvoice_runtime_ready: bool,
    source: Entity<TextField>,
    source_open: bool,
    models_open: bool,
    voice_open: bool,
    preview_open: bool,
    format: Format,
    scroll: ScrollHandle,
    text: String,
    script: Option<Result<Vec<Segment>, String>>,
    preview_count: usize,
    voice_count: usize,
    revision: u64,
    output_revision: u64,
    observed_reference_text: String,
    editing: bool,
    kokoro_voices: Vec<KokoroVoice>,
    voice_category: VoiceCategory,
    voice_filter: Entity<TextField>,
    speaker: i32,
    session: Option<Arc<Session>>,
    use_mic_reference: bool,
    builtin_voices: Vec<String>,
    builtin_voice: String,
    builtin_reference: Option<BuiltinReference>,
    builtin_error: Option<String>,
    loading_builtin: bool,
    recording: Option<Recording>,
    recorded_reference: Option<tempfile::TempPath>,
    reference_text: Entity<TextField>,
    output: Option<Arc<Output>>,
    preview: Option<AudioPreview>,
    job: Option<Arc<Progress>>,
    pub(super) status: Option<String>,
    _observe_voice_filter: Subscription,
    _observe_ref_text: Subscription,
}

impl SpeechTools {
    pub(super) fn any_field_focused(&self, window: &Window, cx: &App) -> bool {
        self.source.focus_handle(cx).is_focused(window)
            || self.voice_filter.focus_handle(cx).is_focused(window)
            || self.reference_text.focus_handle(cx).is_focused(window)
    }

    pub(super) fn new(library: &Library, cx: &mut Context<Workspace>) -> Self {
        let models_root = library.asr_model_preferences().root();
        let model = Model::default();
        let voice_filter = cx.new(|cx| TextField::new(cx, "搜索说话人编号或名称"));
        let reference_text = cx.new(|cx| TextField::new(cx, "输入与录音完全对应的逐字稿"));
        let _observe_voice_filter = cx.observe(&voice_filter, |this, _, cx| {
            this.speech_tools.voice_count = 18;
            cx.notify();
        });
        let _observe_ref_text = cx.observe(&reference_text, |this, field, cx| {
            let text = field.read(cx).text().trim();
            let state = &mut this.speech_tools;
            if state.observed_reference_text != text {
                state.observed_reference_text = text.to_owned();
                state.input_changed();
                cx.notify();
            }
        });
        let builtin_root = tts::builtin_voices_root();
        let builtin_voices = tts::list_builtin_voices(&builtin_root).unwrap_or_default();
        let builtin_voice = builtin_voices.first().cloned().unwrap_or_default();
        let mut state = Self {
            model_check: model.check(&model.dir(&models_root)),
            model_exists: model.dir(&models_root).is_dir(),
            sherpa_runtime_ready: crate::library::sherpa::is_runtime_available(),
            cosyvoice_runtime_ready: tts::cosyvoice_runtime_ready(&models_root),
            models_root,
            family: model.family(),
            model,
            source: cx.new(|cx| TextField::new(cx, "HTTPS/HTTP 下载地址或 ModelScope 仓库")),
            source_open: false,
            models_open: false,
            voice_open: false,
            preview_open: false,
            format: Format::default(),
            scroll: ScrollHandle::new(),
            text: String::new(),
            script: None,
            preview_count: 5,
            voice_count: 18,
            revision: 0,
            output_revision: 0,
            observed_reference_text: String::new(),
            editing: false,
            kokoro_voices: tts::kokoro_voices(),
            voice_category: VoiceCategory::All,
            voice_filter,
            speaker: DEFAULT_KOKORO_SPEAKER,
            session: None,
            use_mic_reference: false,
            builtin_voices,
            builtin_voice,
            builtin_reference: None,
            builtin_error: None,
            loading_builtin: false,
            recording: None,
            recorded_reference: None,
            reference_text,
            output: None,
            preview: None,
            job: None,
            status: None,
            _observe_voice_filter,
            _observe_ref_text,
        };
        state.refresh_source(library, cx);
        state
    }

    fn custom_source(&self, library: &Library) -> Option<String> {
        library.tts_download_urls().get(self.model.key()).cloned()
    }

    fn effective_source(&self, library: &Library) -> String {
        self.custom_source(library).unwrap_or_default()
    }

    fn browse_url(&self, library: &Library) -> String {
        self.custom_source(library)
            .unwrap_or_else(|| self.model.default_browse_url().to_owned())
    }

    fn refresh_source(&mut self, library: &Library, cx: &mut Context<Workspace>) {
        let current = self.effective_source(library);
        self.source
            .update(cx, |field, cx| field.set_text(current, cx));
    }

    pub(super) fn sync_root(&mut self, library: &Library) {
        let root = library.asr_model_preferences().root();
        if self.models_root != root && self.job.is_none() {
            self.models_root = root;
            self.session = None;
            self.refresh_model();
        }
    }

    pub(super) fn refresh_model(&mut self) {
        let directory = self.model.dir(&self.models_root);
        self.model_check = self.model.check(&directory);
        self.model_exists = directory.is_dir();
        self.sherpa_runtime_ready = crate::library::sherpa::is_runtime_available();
        self.cosyvoice_runtime_ready = tts::cosyvoice_runtime_ready(&self.models_root);
    }

    pub(super) fn recording(&self) -> bool {
        self.recording.is_some()
    }

    pub(super) fn stop_preview(&mut self) {
        self.preview = None;
    }

    fn set_text(&mut self, text: String) {
        if self.text != text || self.script.is_none() {
            self.text = text;
            self.reparse();
        }
    }

    fn input_changed(&mut self) {
        self.revision += 1;
        self.status = None;
    }

    fn reparse(&mut self) {
        self.preview_count = 5;
        self.input_changed();
        self.script = (!self.text.trim().is_empty()).then(|| tts::parse(self.format, &self.text));
    }
}

impl Drop for SpeechTools {
    fn drop(&mut self) {
        if let Some(job) = &self.job {
            job.cancel();
        }
    }
}

impl Workspace {
    pub(super) fn poll_speech_tools(&mut self, second_elapsed: bool, cx: &mut Context<Self>) {
        let page = self.view == AppView::Speech && !self.now_playing_open;
        if page {
            self.speech_tools.sync_root(&self.library);
            if self.speech_tools.model.uses_reference()
                && !self.speech_tools.use_mic_reference
                && self.speech_tools.builtin_reference.is_none()
                && self.speech_tools.builtin_error.is_none()
                && !self.speech_tools.loading_builtin
                && !self.speech_tools.builtin_voice.is_empty()
                && self.speech_tools.job.is_none()
            {
                self.load_selected_builtin_voice(cx);
            }
        }
        let state = &mut self.speech_tools;
        if page
            && second_elapsed
            && state.job.is_none()
            && state
                .session
                .as_ref()
                .is_some_and(|session| !session.running())
        {
            state.session = None;
            state.status = Some("语音模型已退出，下次生成时会重新加载".into());
            cx.notify();
        }
        if !page {
            state.recording = None;
            state.stop_preview();
        }
        if state
            .preview
            .as_ref()
            .is_some_and(AudioPreview::is_finished)
        {
            state.stop_preview();
            cx.notify();
        }
        let capture =
            state
                .recording
                .as_ref()
                .and_then(|recording| match recording.result.try_recv() {
                    Ok(result) => Some(result),
                    Err(TryRecvError::Disconnected) => Some(Err("麦克风录音已中断".into())),
                    Err(TryRecvError::Empty) => None,
                });
        if let Some(capture) = capture {
            state.recording = None;
            match capture {
                Ok(capture) => {
                    self.start_speech_job(cx, "正在规范化参考录音", move |_| {
                        let path =
                            tts::prepare_recorded_reference(capture.rate.get(), &capture.mono)?;
                        Ok(Completion::RecordedReference(path))
                    })
                }
                Err(error) => self.speech_tools.status = Some(error),
            }
            cx.notify();
        } else if page && (state.recording.is_some() || state.job.is_some()) {
            cx.notify();
        }
    }

    fn load_selected_builtin_voice(&mut self, cx: &mut Context<Self>) {
        let voice = self.speech_tools.builtin_voice.clone();
        if voice.is_empty() || self.speech_tools.loading_builtin {
            return;
        }
        self.speech_tools.loading_builtin = true;
        self.speech_tools.builtin_error = None;
        self.speech_tools.builtin_reference = None;
        let root = tts::builtin_voices_root();
        let task = cx.background_executor().spawn(async move {
            tts::load_builtin_reference(&root, &voice)
                .map(|reference| Completion::BuiltinReference { voice, reference })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let state = &mut this.speech_tools;
                state.loading_builtin = false;
                match result {
                    Ok(Completion::BuiltinReference { voice, reference }) => {
                        if state.builtin_voice == voice {
                            if reference.text.is_none() {
                                state.builtin_error = Some(
                                    "该内置音色缺少配套逐字稿，无法用于声音克隆，请改用麦克风录制"
                                        .into(),
                                );
                            }
                            state.builtin_reference = Some(reference);
                        }
                    }
                    Ok(_) => {}
                    Err(error) => {
                        state.builtin_error = Some(format!("内置参考音频不可用：{error}"))
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn start_speech_job(
        &mut self,
        cx: &mut Context<Self>,
        label: &'static str,
        task: impl FnOnce(&Progress) -> Result<Completion, String> + Send + 'static,
    ) {
        let state = &mut self.speech_tools;
        if state.job.is_some() || state.recording.is_some() {
            return;
        }
        let job = Arc::new(Progress::default());
        job.set(label);
        state.status = None;
        state.job = Some(job.clone());
        let work = job.clone();
        let task = cx.background_executor().spawn(async move { task(&work) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let state = &mut this.speech_tools;
                state.job = None;
                match result {
                    Ok(Completion::ModelFiles) => {
                        state.refresh_model();
                        state.status = Some(
                            if state.model_check.is_ok() {
                                "模型文件已就绪"
                            } else {
                                "已移除模型文件"
                            }
                            .into(),
                        );
                    }
                    Ok(Completion::RuntimePrepared) => {
                        state.refresh_model();
                        state.status = Some("CosyVoice3 CPU 推理环境已就绪".into());
                    }
                    Ok(Completion::Generated {
                        output,
                        session,
                        revision,
                    }) => {
                        if job.ensure_running().is_ok() {
                            state.session = Some(session);
                            state.output = Some(Arc::new(output));
                            state.output_revision = revision;
                            state.status = None;
                        }
                    }
                    Ok(Completion::RecordedReference(path)) => {
                        state.input_changed();
                        state.recorded_reference = Some(path);
                        state.use_mic_reference = true;
                        state
                            .reference_text
                            .update(cx, |field, cx| field.set_text("", cx));
                        state.status = Some("参考录音已就绪，请填写对应逐字稿".into());
                    }
                    Ok(Completion::BuiltinReference { .. }) => {}
                    Err(error) => state.status = Some(error),
                }
                if let Err(error) = job.ensure_running() {
                    state.status = Some(error);
                }
                if state
                    .session
                    .as_ref()
                    .is_some_and(|session| !session.running())
                {
                    state.session = None;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn edit_speech_text(&mut self, window: &Window, cx: &mut Context<Self>) {
        let state = &mut self.speech_tools;
        if state.editing {
            return;
        }
        let title = if state.format == Format::Txt {
            "输入要朗读的文字".to_string()
        } else {
            format!("编辑 {} 字幕脚本", state.format.label())
        };
        let response = match desktop::edit_subtitle_text(window, &title, &state.text, None) {
            Ok(response) => response,
            Err(error) => {
                state.status = Some(error);
                cx.notify();
                return;
            }
        };
        state.editing = true;
        cx.spawn(async move |this, cx| {
            let result = response.await.ok().flatten();
            this.update(cx, |this, cx| {
                this.speech_tools.editing = false;
                if let Some(text) = result {
                    this.speech_tools.set_text(text);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn choose_speech_text(&mut self, cx: &mut Context<Self>) {
        let selection = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("导入 TXT、SRT 或 LRC".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = selection.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let task = cx.background_executor().spawn(async move {
                let format = Format::from_path(&path).ok_or("仅支持 TXT、SRT 和 LRC 文件")?;
                let mut text = String::new();
                File::open(path)
                    .and_then(|file| file.take(256 * 1024 + 1).read_to_string(&mut text))
                    .map_err(|error| format!("无法读取 UTF-8 文本：{error}"))?;
                if text.len() > 256 * 1024 {
                    return Err("合成文本最多 256 KiB".into());
                }
                Ok::<_, String>((format, text))
            });
            let result = task.await;
            this.update(cx, |this, cx| {
                let state = &mut this.speech_tools;
                match result {
                    Ok((format, text)) => {
                        state.format = format;
                        state.text = text;
                        state.reparse();
                    }
                    Err(error) => state.status = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_speech_source(&mut self, restore: bool, cx: &mut Context<Self>) {
        let key = self.speech_tools.model.key();
        let model = self.speech_tools.model;
        let source = self.speech_tools.source.read(cx).text().trim().to_owned();
        if restore {
            match self.library.set_tts_download_url(key, None) {
                Ok(()) => {
                    self.speech_tools.refresh_source(&self.library, cx);
                    self.speech_tools.status = Some("已恢复默认下载源".into());
                }
                Err(error) => self.speech_tools.status = Some(error),
            }
            cx.notify();
            return;
        }
        if let Err(error) = tts::validate_source(model, &source) {
            self.speech_tools.status = Some(error);
            cx.notify();
            return;
        }
        let value = (!source.is_empty()).then_some(source.as_str());
        match self.library.set_tts_download_url(key, value) {
            Ok(()) => {
                self.speech_tools.refresh_source(&self.library, cx);
                self.speech_tools.status = Some(
                    if value.is_some() {
                        "已保存下载源"
                    } else {
                        "已恢复默认下载源"
                    }
                    .into(),
                );
            }
            Err(error) => self.speech_tools.status = Some(error),
        }
        cx.notify();
    }

    fn start_speech_recording(&mut self, cx: &mut Context<Self>) {
        if self.microphone_active() || self.speech_tools.job.is_some() {
            return;
        }
        self.speech_tools.stop_preview();
        self.audio.pause();
        self.playing = false;
        match Recording::start(5, 15) {
            Ok(recording) => self.speech_tools.recording = Some(recording),
            Err(error) => self.speech_tools.status = Some(error),
        }
        cx.notify();
    }

    fn speech_reference_blocker<'a>(&'a self, cx: &App) -> Option<&'a str> {
        let state = &self.speech_tools;
        if !state.model.uses_reference() {
            return None;
        }
        if state.use_mic_reference {
            if state.recorded_reference.is_none() {
                return Some("请先录制 5–15 秒参考音色");
            }
            let text = state.reference_text.read(cx).text().trim();
            if !tts::valid_reference_text(text) {
                return Some("请填写与参考录音完全对应的逐字稿");
            }
            return None;
        }
        if let Some(error) = state.builtin_error.as_deref() {
            return Some(error);
        }
        if state.builtin_voice.is_empty() {
            return Some("没有可用的内置音色，请录制参考音色。");
        }
        let Some(reference) = &state.builtin_reference else {
            return Some("正在准备内置音色，也可使用麦克风录制。");
        };
        if reference
            .text
            .as_deref()
            .is_none_or(|text| !tts::valid_reference_text(text.trim()))
        {
            return Some("该内置音色缺少配套逐字稿，请改用麦克风录制");
        }
        None
    }

    fn synthesize_speech(&mut self, cx: &mut Context<Self>) {
        if let Some(blocker) = self.speech_reference_blocker(cx) {
            self.speech_tools.status = Some(blocker.into());
            cx.notify();
            return;
        }
        let state = &mut self.speech_tools;
        let session = state.session.clone();
        let model = state.model;
        let root = state.models_root.clone();
        let revision = state.revision;
        let Some(Ok(script)) = &state.script else {
            return;
        };
        let speaker = state.speaker;
        let reference = if model.uses_reference() {
            if state.use_mic_reference {
                let path = state.recorded_reference.as_ref().map(|p| p.to_path_buf());
                let text = state.reference_text.read(cx).text().trim().to_owned();
                path.map(|path| Reference { path, text })
            } else {
                state.builtin_reference.as_ref().and_then(|builtin| {
                    builtin.text.as_ref().map(|text| Reference {
                        path: builtin.file.to_path_buf(),
                        text: text.clone(),
                    })
                })
            }
        } else {
            None
        };
        let script = script.clone();
        state.stop_preview();
        self.start_speech_job(cx, "正在准备合成", move |progress| {
            let session =
                match session.filter(|session| session.model == model && session.running()) {
                    Some(session) => session,
                    None => {
                        progress.set("正在加载语音模型");
                        Arc::new(
                            Session::load(model, &root, progress)
                                .map_err(|error| format!("无法加载语音模型：{error}"))?,
                        )
                    }
                };
            let output = session
                .synthesize(script, speaker, reference, progress)
                .map_err(|error| format!("生成语音失败：{error}"))?;
            Ok(Completion::Generated {
                output,
                session,
                revision,
            })
        });
    }

    fn preview_speech(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.microphone_active() {
            return;
        }
        self.speech_tools.stop_preview();
        match AudioPreview::prepare(&path, self.output_volume()) {
            Ok(preview) => {
                self.audio.pause();
                self.playing = false;
                preview.play();
                self.speech_tools.preview = Some(preview);
                self.speech_tools.status = None;
            }
            Err(error) => self.speech_tools.status = Some(format!("试听失败：{error}")),
        }
        cx.notify();
    }

    fn save_speech(&mut self, cx: &mut Context<Self>) {
        let Some(output) = self.speech_tools.output.clone() else {
            return;
        };
        let directory = dirs::download_dir().unwrap_or_else(|| self.library.root().to_path_buf());
        let answer = cx.prompt_for_new_path(&directory, Some("合成语音.wav"));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = answer.await else {
                return;
            };
            let task = cx.background_executor().spawn(async move {
                tts::export(&output.file, &path)
                    .map(|()| format!("已保存 {}", path.display()))
                    .unwrap_or_else(|error| format!("保存 WAV 失败：{error}"))
            });
            let message = task.await;
            this.update(cx, |this, cx| {
                this.speech_tools.status = Some(message);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn dismiss_speech_options(&mut self, cx: &mut Context<Self>) -> bool {
        let state = &mut self.speech_tools;
        let open = if state.source_open && state.models_open {
            &mut state.source_open
        } else if state.models_open {
            &mut state.models_open
        } else if state.voice_open && state.recording.is_none() {
            &mut state.voice_open
        } else {
            return false;
        };
        *open = false;
        cx.notify();
        true
    }

    fn install_speech_model(&mut self, cx: &mut Context<Self>) {
        let state = &self.speech_tools;
        let source = state.effective_source(&self.library);
        let model = state.model;
        let root = state.models_root.clone();
        self.start_speech_job(cx, "正在下载语音模型", move |progress| {
            tts::install(&root, model, &source, progress)?;
            Ok(Completion::ModelFiles)
        });
    }

    fn prepare_speech_runtime(&mut self, cx: &mut Context<Self>) {
        let root = self.speech_tools.models_root.clone();
        let model_dir = Model::CosyVoice3.dir(&root);
        self.start_speech_job(cx, "正在准备语音运行环境", move |progress| {
            tts::prepare_cosyvoice_runtime(&root, Some(&model_dir), progress)?;
            Ok(Completion::RuntimePrepared)
        });
    }

    fn speech_models_panel(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let state = &self.speech_tools;
        let busy = state.job.is_some() || state.recording.is_some();
        let loaded = state.session.is_some();
        let model_exists = state.model_exists;
        let family = state.family;
        let model = state.model;

        let family_tabs = Family::ALL.map(|candidate| {
            let active = family == candidate;
            div()
                .id(SharedString::from(format!(
                    "tts-family-{}",
                    candidate.label()
                )))
                .flex_1()
                .px_3()
                .py_2()
                .rounded_md()
                .cursor_pointer()
                .bg(if active {
                    fade(theme_color(ACCENT), 0.13)
                } else {
                    fade(theme_color(CONTROL_TINT), 0.04)
                })
                .border_1()
                .border_color(if active {
                    fade(theme_color(ACCENT), 0.45)
                } else {
                    fade(theme_color(LINE), 1.0)
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    let state = &mut this.speech_tools;
                    if state.family != candidate && state.job.is_none() {
                        state.family = candidate;
                        let first = Model::by_family(candidate)[0];
                        state.session = None;
                        state.model = first;
                        state.input_changed();
                        state.refresh_model();
                        state.refresh_source(&this.library, cx);
                    }
                    cx.notify();
                }))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(if active {
                            theme_color(ACCENT)
                        } else {
                            theme_color(TEXT)
                        })
                        .child(candidate.label()),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme_color(MUTED))
                        .child(candidate.subtitle()),
                )
        });

        let variants = Model::by_family(family)
            .into_iter()
            .map(|candidate| {
                let label = if candidate.recommended() {
                    format!(
                        "{} · {:.0} MB · 推荐",
                        candidate.label(),
                        candidate.size_bytes() as f64 / 1_000_000.0
                    )
                } else {
                    format!(
                        "{} · {:.0} MB",
                        candidate.label(),
                        candidate.size_bytes() as f64 / 1_000_000.0
                    )
                };
                self.hero_button(
                    candidate.key(),
                    "disc",
                    &label,
                    model == candidate,
                    cx,
                    move |this, _, cx| {
                        let state = &mut this.speech_tools;
                        if state.model != candidate && state.job.is_none() {
                            state.session = None;
                            state.model = candidate;
                            state.input_changed();
                            state.refresh_model();
                            state.refresh_source(&this.library, cx);
                        }
                        cx.notify();
                    },
                )
            })
            .collect::<Vec<_>>();

        let state = &self.speech_tools;
        let model_ready = state.model_check.is_ok();
        let show_runtime = state.model == Model::CosyVoice3;
        let show_sherpa_runtime = !show_runtime;
        let sherpa_ready = state.sherpa_runtime_ready;
        let sherpa_install = self.sherpa_install_status();
        let runtime_ready = state.cosyvoice_runtime_ready;
        let source_open = state.source_open;
        let source_field = state.source.clone();
        let (model_badge, model_badge_accent, model_status) = match &state.model_check {
            Ok(()) if loaded => (
                format!(
                    "CPU · {} Hz",
                    state.session.as_ref().map_or(24000, |s| s.rate)
                ),
                true,
                "模型已加载，可立即开始合成".into(),
            ),
            Ok(()) if show_sherpa_runtime && !sherpa_ready => (
                "缺推理库".into(),
                false,
                "模型文件已就绪，需先安装通用推理库（sherpa-onnx 2.0.2）".into(),
            ),
            Ok(()) => ("已就绪".into(), true, "文件已就绪，生成时会加载模型".into()),
            Err(error) => ("未就绪".into(), false, error.clone()),
        };

        tool_panel()
            .child(tool_header_badge(
                "模型管理",
                model_status,
                model_badge,
                model_badge_accent,
            ))
            .child(div().flex().gap_2().children(family_tabs))
            .when(show_runtime, |panel| {
                panel.child(tool_message(if runtime_ready {
                    "CosyVoice3 本地运行环境已就绪".into()
                } else {
                    "CosyVoice3 需要下载独立运行环境（uv 与 Python 3.11）".into()
                }))
            })
            .when(show_sherpa_runtime && !sherpa_ready, |panel| {
                panel
                    .child(if let Some((ref message, ratio)) = sherpa_install {
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(tool_banner(message.clone(), false))
                            .children(ratio.map(tool_progress_bar))
                    } else {
                        div().child(tool_banner(
                            format!(
                                "未安装 sherpa-onnx 2.0.2 通用推理库（约 {:.1} MB，与字幕、人声分离共用）。",
                                crate::library::sherpa::current_package()
                                    .map_or(9_000_000.0, |pkg| pkg.size_bytes as f64)
                                    / 1_000_000.0
                            ),
                            true,
                        ))
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .flex_wrap()
                            .when(sherpa_install.is_some(), |row| {
                                row.child(self.tiny_button(
                                    "取消安装",
                                    "speech-models-cancel-sherpa",
                                    cx,
                                    |this, _, cx| this.cancel_sherpa_runtime_install(cx),
                                ))
                            })
                            .when(sherpa_install.is_none(), |row| {
                                row.child(self.hero_button(
                                    "speech-models-install-sherpa",
                                    "download",
                                    "一键安装推理库",
                                    true,
                                    cx,
                                    |this, _, cx| this.start_sherpa_runtime_install(cx),
                                ))
                                .child(self.tiny_button(
                                    "打开模型设置",
                                    "speech-models-open-settings",
                                    cx,
                                    |this, window, cx| {
                                        this.open_settings_page(SettingsPage::Models, window, cx)
                                    },
                                ))
                            }),
                    )
            })
            .when(!busy, |panel| {
                panel
                    .child(div().flex().gap_2().flex_wrap().children(variants))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .flex_wrap()
                            .when(!model_ready, |row| {
                                row.child(self.tiny_button(
                                    if model_exists {
                                        "补齐模型"
                                    } else {
                                        "下载模型"
                                    },
                                    "install-tts-from-models",
                                    cx,
                                    |this, _, cx| this.install_speech_model(cx),
                                ))
                            })
                            .when(model_ready && show_runtime && !runtime_ready, |row| {
                                row.child(self.tiny_button(
                                    "准备运行环境",
                                    "prepare-tts-from-models",
                                    cx,
                                    |this, _, cx| this.prepare_speech_runtime(cx),
                                ))
                            })
                            .when(loaded, |row| {
                                row.child(self.tiny_button(
                                    "释放模型",
                                    "release-tts-model",
                                    cx,
                                    |this, _, cx| {
                                        this.speech_tools.session = None;
                                        this.speech_tools.status = Some("已释放语音模型".into());
                                        cx.notify();
                                    },
                                ))
                            })
                            .child(self.tiny_button(
                                "重新检查",
                                "check-tts-files",
                                cx,
                                |this, _, cx| {
                                    this.speech_tools.refresh_model();
                                    cx.notify();
                                },
                            ))
                            .child(self.tiny_button(
                                "模型目录",
                                "open-tts-directory",
                                cx,
                                |this, _, cx| {
                                    let dir = this.speech_tools.models_root.join("tts");
                                    match fs::create_dir_all(&dir) {
                                        Ok(()) => cx.reveal_path(&dir),
                                        Err(error) => {
                                            this.speech_tools.status = Some(error.to_string())
                                        }
                                    }
                                    cx.notify();
                                },
                            ))
                            .child(self.tiny_button(
                                "下载源",
                                "toggle-tts-source",
                                cx,
                                |this, _, cx| {
                                    this.speech_tools.source_open = !this.speech_tools.source_open;
                                    cx.notify();
                                },
                            ))
                            .when(!loaded && model_exists, |row| {
                                row.child(self.tiny_button(
                                    "移除文件",
                                    "remove-tts-model",
                                    cx,
                                    |this, window, cx| this.confirm_remove_speech_model(window, cx),
                                ))
                            }),
                    )
                    .when(source_open, |panel| {
                        panel.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(tool_message(format!(
                                    "默认源：{}（留空即使用内置默认源）",
                                    model.default_source_url()
                                )))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(div().flex_1().min_w(px(0.)).child(source_field))
                                        .child(self.tiny_button(
                                            "保存",
                                            "save-tts-source",
                                            cx,
                                            |this, _, cx| this.set_speech_source(false, cx),
                                        ))
                                        .child(self.tiny_button(
                                            "恢复默认",
                                            "reset-tts-source",
                                            cx,
                                            |this, _, cx| this.set_speech_source(true, cx),
                                        ))
                                        .child(self.tiny_button(
                                            "访问下载源",
                                            "open-tts-source",
                                            cx,
                                            |this, _, cx| {
                                                let url =
                                                    this.speech_tools.browse_url(&this.library);
                                                cx.open_url(&url);
                                            },
                                        )),
                                ),
                        )
                    })
            })
    }

    fn speech_script_panel(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let state = &self.speech_tools;
        let format = state.format;
        let preview_open = state.preview_open;
        let preview_count = state.preview_count;
        let segments = state
            .script
            .as_ref()
            .and_then(|result| result.as_ref().ok());
        let has_text = !state.text.trim().is_empty();
        let excerpt = state.text.trim().chars().take(280).collect::<String>();
        let subtitle = segments.map_or_else(
            || "输入文字，或导入 TXT、SRT、LRC 文件。".to_string(),
            |lines| {
                format!(
                    "{} · {} 段 · {} 字符",
                    format.label(),
                    lines.len(),
                    state.text.chars().count()
                )
            },
        );
        let preview = preview_open.then(|| {
            segments
                .into_iter()
                .flatten()
                .take(preview_count)
                .enumerate()
                .map(|(index, line)| {
                    div()
                        .flex()
                        .gap_3()
                        .py_1p5()
                        .border_b_1()
                        .border_color(theme_color(LINE))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme_color(MUTED))
                                .child(format!("{}", index + 1)),
                        )
                        .children(line.start.map(|start| {
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(theme_color(MUTED))
                                .child(line.end.map_or_else(
                                    || format!("{start:.2}s"),
                                    |end| format!("{start:.2}–{end:.2}s"),
                                ))
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .text_sm()
                                .child(line.text.clone()),
                        )
                })
                .collect::<Vec<_>>()
        });
        let can_show_more =
            preview_open && segments.is_some_and(|lines| lines.len() > preview_count);
        let can_preview = segments.is_some();
        let error = state
            .script
            .as_ref()
            .and_then(|result| result.as_ref().err())
            .cloned();

        tool_panel()
            .child(tool_header("脚本", subtitle))
            .child(
                div()
                    .id("speech-script-area")
                    .px_4()
                    .py_3()
                    .min_h(px(82.))
                    .rounded_md()
                    .border_1()
                    .border_color(theme_color(LINE))
                    .bg(theme_color(BG))
                    .cursor_text()
                    .hover(|style| style.border_color(theme_color(ACCENT)))
                    .on_click(cx.listener(|this, _, window, cx| this.edit_speech_text(window, cx)))
                    .child(
                        div()
                            .line_clamp(3)
                            .text_sm()
                            .text_color(theme_color(if has_text { TEXT } else { MUTED }))
                            .child(if has_text {
                                excerpt
                            } else {
                                "点击输入要朗读的文字…".into()
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .flex_wrap()
                    .items_center()
                    .when(has_text, |row| {
                        row.child(self.tool_button(
                            "edit-speech-text",
                            "edit",
                            "编辑脚本",
                            false,
                            cx,
                            |this, window, cx| this.edit_speech_text(window, cx),
                        ))
                    })
                    .when(!has_text, |row| {
                        row.child(self.tiny_button(
                            "粘贴文字",
                            "paste-tts-text",
                            cx,
                            |this, _, cx| {
                                let text = cx
                                    .read_from_clipboard()
                                    .and_then(|clip| clip.text())
                                    .unwrap_or_default();
                                if text.len() <= 256 * 1024 && !text.trim().is_empty() {
                                    this.speech_tools.set_text(text);
                                } else {
                                    this.speech_tools.status =
                                        Some("剪贴板没有可用文字，或内容超过 256 KiB。".into());
                                }
                                cx.notify();
                            },
                        ))
                    })
                    .child(self.tiny_button(
                        "导入文件…",
                        "import-tts-text",
                        cx,
                        |this, _, cx| this.choose_speech_text(cx),
                    ))
                    .child(div().flex_1())
                    .when(can_preview, |row| {
                        row.child(self.tiny_button(
                            if preview_open {
                                "收起分段"
                            } else {
                                "预览分段"
                            },
                            "speech-preview-toggle",
                            cx,
                            |this, _, cx| {
                                this.speech_tools.preview_open = !this.speech_tools.preview_open;
                                cx.notify();
                            },
                        ))
                    }),
            )
            .when(has_text, |panel| {
                panel.child(
                    div()
                        .flex()
                        .gap_2()
                        .items_center()
                        .flex_wrap()
                        .child(tool_message("脚本格式".into()))
                        .child(super::controls::segmented_control(
                            "tts-script-format",
                            &[
                                (Format::Txt, "纯文本"),
                                (Format::Srt, "SRT"),
                                (Format::Lrc, "LRC"),
                            ],
                            format,
                            self.keyboard_focus_visible,
                            cx,
                            |this, candidate, _, cx| {
                                if this.speech_tools.format != candidate {
                                    this.speech_tools.format = candidate;
                                    this.speech_tools.reparse();
                                    cx.notify();
                                }
                            },
                        )),
                )
            })
            .children(error.map(|error| tool_banner(error, true)))
            .children(preview.into_iter().flatten())
            .when(can_show_more, |panel| {
                panel.child(self.tiny_button(
                    "显示更多分段",
                    "speech-preview-more",
                    cx,
                    |this, _, cx| {
                        this.speech_tools.preview_count += 15;
                        cx.notify();
                    },
                ))
            })
    }

    pub(super) fn speech_page(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let state = &self.speech_tools;
        let busy = state.job.is_some() || state.recording.is_some();
        let voice_open = state.voice_open;
        let models_open = state.models_open;
        let model = state.model;
        let sherpa_runtime_ready = state.sherpa_runtime_ready;
        let voice_name = if model.uses_reference() {
            if state.use_mic_reference {
                "麦克风录音".into()
            } else if state.builtin_voice.is_empty() {
                "选择参考音色".into()
            } else {
                state.builtin_voice.trim_end_matches(".wav").to_owned()
            }
        } else {
            state
                .kokoro_voices
                .iter()
                .find(|voice| voice.id == state.speaker)
                .map_or_else(
                    || "默认音色".into(),
                    |voice| tts::kokoro_voice_label(*voice),
                )
        };
        let voice = tool_panel().child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .child(tool_header("音色", voice_name)),
                )
                .when(!busy, |row| {
                    row.child(self.tiny_button(
                        if voice_open {
                            "收起音色"
                        } else {
                            "选择音色"
                        },
                        "toggle-tts-voice",
                        cx,
                        |this, _, cx| {
                            let state = &mut this.speech_tools;
                            state.voice_open = !state.voice_open;
                            if state.voice_open {
                                state.scroll.scroll_to_top_of_item(2);
                            }
                            cx.notify();
                        },
                    ))
                }),
        );
        let (script, voice_options, models, delivery) = (
            self.speech_script_panel(cx),
            voice_open.then(|| self.speech_voice_panel(cx)),
            models_open.then(|| self.speech_models_panel(cx)),
            self.speech_delivery_bar(cx),
        );
        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .child(
                tool_page("speech-tools-page", "文生语音")
                    .track_scroll(&self.speech_tools.scroll)
                    .child(script)
                    .child(voice)
                    .children(voice_options)
                    .child(
                        div()
                            .px_4()
                            .py_2p5()
                            .rounded_lg()
                            .border_1()
                            .border_color(theme_color(LINE))
                            .bg(theme_color(PANEL))
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(self.tiny_button(
                                        if models_open {
                                            "收起模型管理"
                                        } else {
                                            "模型管理"
                                        },
                                        "toggle-tts-models",
                                        cx,
                                        |this, _, cx| {
                                            this.speech_tools.models_open =
                                                !this.speech_tools.models_open;
                                            cx.notify();
                                        },
                                    ))
                                    .child(div().text_xs().text_color(theme_color(MUTED)).child(
                                        if model != Model::CosyVoice3 && !sherpa_runtime_ready {
                                            format!("{} · 未安装推理库", model.label())
                                        } else {
                                            model.label().to_owned()
                                        },
                                    )),
                            )
                            .when(model != Model::CosyVoice3 && !sherpa_runtime_ready, |row| {
                                row.child(self.tiny_button(
                                    "打开模型设置",
                                    "speech-card-open-settings",
                                    cx,
                                    |this, window, cx| {
                                        this.open_settings_page(SettingsPage::Models, window, cx);
                                    },
                                ))
                            }),
                    )
                    .children(models),
            )
            .child(delivery)
            .into_any_element()
    }

    fn speech_delivery_bar(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let sherpa_install = self.sherpa_install_status();
        let state = &self.speech_tools;
        let job = state.job.clone();
        let recording = state.recording.is_some();
        let previous = state.output_revision != state.revision;
        let output = state.output.clone().filter(|_| job.is_none() && !recording);
        let current_result = output.is_some() && !previous;
        let previewing = state.preview.is_some();
        let stop_other_preview = previewing && output.is_none();
        let model_exists = state.model_exists;
        let model = state.model;
        let uses_sherpa = model != Model::CosyVoice3;
        let sherpa_missing = uses_sherpa && !state.sherpa_runtime_ready;
        let runtime_package_mb = crate::library::sherpa::current_package()
            .map_or(9.0, |pkg| pkg.size_bytes as f64 / 1_000_000.0);
        let action = if job.is_some() || recording || state.editing || sherpa_install.is_some() {
            None
        } else if sherpa_missing && state.model_check.is_ok() {
            Some((
                "安装推理库",
                "download",
                format!(
                    "需要安装 sherpa-onnx 2.0.2 通用推理库（约 {runtime_package_mb:.1} MB，与字幕和人声分离共用）。"
                ),
                DeliveryAction::InstallSherpaRuntime,
            ))
        } else if state.script.is_none() {
            Some((
                "输入文字",
                "edit",
                "写好脚本后即可生成音频。".into(),
                DeliveryAction::Edit,
            ))
        } else if state.script.as_ref().is_some_and(Result::is_err) {
            Some((
                "修改脚本",
                "edit",
                "请修正脚本中的格式问题。".into(),
                DeliveryAction::Edit,
            ))
        } else if state.model_check.is_err() {
            Some((
                if model_exists {
                    "补齐模型"
                } else {
                    "下载语音模型"
                },
                "download",
                format!(
                    "首次使用 {} 需要下载约 {:.0} MB。",
                    model.label(),
                    model.size_bytes() as f64 / 1_000_000.
                ),
                DeliveryAction::Install,
            ))
        } else if sherpa_missing {
            Some((
                "安装推理库",
                "download",
                format!(
                    "需要安装 sherpa-onnx 2.0.2 通用推理库（约 {runtime_package_mb:.1} MB，与字幕和人声分离共用）。"
                ),
                DeliveryAction::InstallSherpaRuntime,
            ))
        } else if model == Model::CosyVoice3 && !state.cosyvoice_runtime_ready {
            Some((
                "准备运行环境",
                "download",
                "此音色需要下载 CosyVoice3 本地运行环境。".into(),
                DeliveryAction::PrepareRuntime,
            ))
        } else if let Some(hint) = self.speech_reference_blocker(cx) {
            Some((
                "设置参考音色",
                "music",
                hint.into(),
                DeliveryAction::ChooseVoice,
            ))
        } else {
            Some((
                "生成语音",
                "audio-lines",
                "生成后可试听并保存 WAV。".into(),
                DeliveryAction::Generate,
            ))
        };
        let can_regenerate =
            current_result && matches!(action, Some((_, _, _, DeliveryAction::Generate)));
        let action = action.filter(|_| !can_regenerate);
        let status = state.status.clone();
        let progress = sherpa_install
            .as_ref()
            .map(|(title, ratio)| (title.clone(), *ratio))
            .or_else(|| {
                job.as_ref().map(|job| {
                    let total = job.total.load(Ordering::Relaxed);
                    let received = job.received.load(Ordering::Relaxed);
                    let ratio = (total > 0).then(|| (received as f32 / total as f32).clamp(0., 1.));
                    let title = if job.ensure_running().is_err() {
                        "正在取消…".into()
                    } else {
                        job.text()
                    };
                    (title, ratio)
                })
            });
        let hint = progress
            .as_ref()
            .map(|(title, _)| title.clone())
            .or_else(|| recording.then(|| "正在录制参考音色…".to_string()))
            .or_else(|| {
                action.as_ref().and_then(|(_, _, hint, action)| {
                    matches!(
                        action,
                        DeliveryAction::Install
                            | DeliveryAction::InstallSherpaRuntime
                            | DeliveryAction::PrepareRuntime
                    )
                    .then(|| {
                        status
                            .as_ref()
                            .map_or_else(|| hint.clone(), |status| format!("{status} · {hint}"))
                    })
                })
            })
            .or(status)
            .or_else(|| action.as_ref().map(|(_, _, hint, _)| hint.clone()))
            .unwrap_or_default();
        let show_model_settings = sherpa_missing && sherpa_install.is_none();
        let show_task_row = !hint.is_empty() || show_model_settings || stop_other_preview;

        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap_2()
            .px_6()
            .py_3()
            .bg(theme_color(PANEL))
            .border_t_1()
            .border_color(theme_color(LINE))
            .children(
                progress
                    .as_ref()
                    .and_then(|(_, ratio)| ratio.map(tool_progress_bar)),
            )
            .when_some(output, |bar, output| {
                let path = output.file.to_path_buf();
                bar.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .when(show_task_row, |row| {
                            row.pb_2().border_b_1().border_color(theme_color(LINE))
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .text_sm()
                                .truncate()
                                .child(format!(
                                    "{} · WAV · {} Hz · {:.1} 秒",
                                    if previous {
                                        "上次生成"
                                    } else {
                                        "生成结果"
                                    },
                                    output.rate,
                                    output.duration.as_secs_f64(),
                                )),
                        )
                        .when(can_regenerate, |row| {
                            row.child(self.tiny_button(
                                "重新生成",
                                "regenerate-tts-wav",
                                cx,
                                |this, _, cx| this.synthesize_speech(cx),
                            ))
                        })
                        .child(self.tiny_button(
                            "保存 WAV…",
                            "save-tts-wav",
                            cx,
                            |this, _, cx| this.save_speech(cx),
                        ))
                        .child(
                            controls::ui_button(
                                "toggle-tts-preview",
                                if previewing { "停止试听" } else { "试听" },
                                (!previewing).then_some("play-fill"),
                                if current_result {
                                    controls::ButtonTone::Primary
                                } else {
                                    controls::ButtonTone::Secondary
                                },
                                true,
                                !current_result,
                                self.keyboard_focus_visible,
                                cx,
                                move |this, _, cx| {
                                    if this.speech_tools.preview.is_some() {
                                        this.speech_tools.stop_preview();
                                        cx.notify();
                                    } else {
                                        this.preview_speech(path.clone(), cx);
                                    }
                                },
                            )
                            .min_w(px(84.)),
                        ),
                )
            })
            .when(show_task_row, |bar| {
                bar.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .text_sm()
                                .text_color(theme_color(MUTED))
                                .line_clamp(2)
                                .child(hint),
                        )
                        .when(stop_other_preview, |row| {
                            row.child(self.tiny_button(
                                "停止试听",
                                "stop-speech-preview",
                                cx,
                                |this, _, cx| {
                                    this.speech_tools.stop_preview();
                                    cx.notify();
                                },
                            ))
                        })
                        .when(sherpa_install.is_some(), |row| {
                            row.child(self.tiny_button(
                                "取消安装",
                                "cancel-sherpa-from-speech",
                                cx,
                                |this, _, cx| this.cancel_sherpa_runtime_install(cx),
                            ))
                        })
                        .when(show_model_settings, |row| {
                            row.child(self.tiny_button(
                                "模型设置",
                                "speech-delivery-open-settings",
                                cx,
                                |this, window, cx| {
                                    this.open_settings_page(SettingsPage::Models, window, cx)
                                },
                            ))
                        })
                        .when(job.is_some_and(|job| job.ensure_running().is_ok()), |row| {
                            row.child(self.tiny_button(
                                "取消任务",
                                "cancel-tts-job",
                                cx,
                                |this, _, cx| {
                                    if let Some(job) = &this.speech_tools.job {
                                        job.cancel();
                                    }
                                    cx.notify();
                                },
                            ))
                        })
                        .when_some(action, |row, (label, icon, _, action)| {
                            row.child(self.hero_button(
                                "speech-primary-action",
                                icon,
                                label,
                                !current_result,
                                cx,
                                move |this, window, cx| match action {
                                    DeliveryAction::Edit => this.edit_speech_text(window, cx),
                                    DeliveryAction::Install => this.install_speech_model(cx),
                                    DeliveryAction::InstallSherpaRuntime => {
                                        this.start_sherpa_runtime_install(cx)
                                    }
                                    DeliveryAction::PrepareRuntime => {
                                        this.prepare_speech_runtime(cx)
                                    }
                                    DeliveryAction::ChooseVoice => {
                                        this.speech_tools.voice_open = true;
                                        this.speech_tools.scroll.scroll_to_top_of_item(2);
                                        cx.notify();
                                    }
                                    DeliveryAction::Generate => this.synthesize_speech(cx),
                                },
                            ))
                        }),
                )
            })
    }

    fn speech_voice_panel(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let state = &self.speech_tools;
        if !state.model.uses_reference() {
            let selected = state.speaker;
            let available = state
                .session
                .as_ref()
                .map_or(i32::MAX, |session| session.speakers);
            let category = state.voice_category;
            let query = state
                .voice_filter
                .read(cx)
                .text()
                .trim()
                .to_ascii_lowercase();
            let filter_field = state.voice_filter.clone();
            let voice_count = state.voice_count;
            let filtered = state
                .kokoro_voices
                .iter()
                .copied()
                .filter(|voice| voice.id < available)
                .filter(|voice| category == VoiceCategory::All || voice.category == category)
                .filter(|voice| {
                    query.is_empty()
                        || voice.code.contains(&query)
                        || tts::kokoro_voice_label(*voice)
                            .to_ascii_lowercase()
                            .contains(&query)
                })
                .collect::<Vec<_>>();
            let result_count = filtered.len();
            let categories = VoiceCategory::ALL.map(|candidate| {
                super::controls::option_chip(
                    SharedString::from(format!("voice-cat-{}", candidate.label())),
                    candidate.label(),
                    category == candidate,
                    true,
                    self.keyboard_focus_visible,
                    cx,
                    move |this, _, cx| {
                        this.speech_tools.voice_category = candidate;
                        this.speech_tools.voice_count = 18;
                        cx.notify();
                    },
                )
            });
            let buttons = filtered
                .into_iter()
                .take(voice_count)
                .map(|voice| {
                    let label = tts::kokoro_voice_label(voice);
                    super::controls::option_chip(
                        SharedString::from(format!("kokoro-voice-{}", voice.code)),
                        label,
                        voice.id == selected,
                        true,
                        self.keyboard_focus_visible,
                        cx,
                        move |this, _, cx| {
                            let state = &mut this.speech_tools;
                            if state.speaker != voice.id {
                                state.speaker = voice.id;
                                state.input_changed();
                            }
                            state.voice_open = false;
                            cx.notify();
                        },
                    )
                })
                .collect::<Vec<_>>();
            return tool_panel()
                .child(tool_header_badge(
                    "内置说话人",
                    "可按语言、名称或编号查找。",
                    format!("{result_count} 个"),
                    false,
                ))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .flex_wrap()
                        .children(categories)
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(160.))
                                .child(super::audio_tools::tool_text_input(filter_field)),
                        ),
                )
                .child(div().flex().flex_wrap().gap_2().children(buttons))
                .when(result_count == 0, |panel| {
                    panel.child(tool_message("没有匹配的音色，请换个关键词或类别。".into()))
                })
                .when(result_count > voice_count, |panel| {
                    panel.child(self.tiny_button(
                        "显示更多音色",
                        "more-kokoro-voices",
                        cx,
                        |this, _, cx| {
                            this.speech_tools.voice_count += 18;
                            cx.notify();
                        },
                    ))
                });
        }

        let elapsed = state
            .recording
            .as_ref()
            .map(|recording| recording.elapsed.load(Ordering::Relaxed));
        let use_mic = state.use_mic_reference;
        let builtin_voices = state.builtin_voices.clone();
        let selected_builtin = state.builtin_voice.clone();
        let builtin_ref_path = state
            .builtin_reference
            .as_ref()
            .map(|item| item.file.to_path_buf());
        let builtin_transcript = state
            .builtin_reference
            .as_ref()
            .and_then(|item| item.text.clone());
        let builtin_error = state.builtin_error.clone();
        let recorded = state
            .recorded_reference
            .as_ref()
            .map(|path| path.to_path_buf());
        let field = state.reference_text.clone();
        let busy = state.job.is_some();

        let mut panel = tool_panel().child(tool_header(
            "参考音色",
            "选择应用内置参考音色，或用麦克风录制 5–15 秒人声并填写逐字稿。",
        ));

        if let Some(elapsed) = elapsed {
            let seconds = elapsed as f64 / 1000.0;
            let ready = elapsed >= 5_000;
            return panel
                .child(tool_message(format!("推荐朗读：{RECORDING_PROMPT}")))
                .child(tool_progress_bar((seconds / 15.0).clamp(0.0, 1.0) as f32))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .items_center()
                        .flex_wrap()
                        .child(tool_message(if elapsed == 0 {
                            "正在等待麦克风输入…".into()
                        } else if !ready {
                            format!(
                                "录制 {seconds:.1} / 15.0 秒（还需 {:.1} 秒可结束）",
                                5.0 - seconds
                            )
                        } else {
                            format!("录制 {seconds:.1} / 15.0 秒 · 可随时结束")
                        }))
                        .when(ready, |row| {
                            row.child(self.hero_button(
                                "finish-tts-reference",
                                "check",
                                "结束录音",
                                true,
                                cx,
                                |this, _, _| {
                                    if let Some(recording) = &this.speech_tools.recording {
                                        recording.finish();
                                    }
                                },
                            ))
                        })
                        .child(self.tiny_button(
                            "取消",
                            "cancel-tts-record",
                            cx,
                            |this, _, cx| {
                                this.speech_tools.recording = None;
                                cx.notify();
                            },
                        )),
                );
        }

        if !busy {
            panel = panel.child(div().flex().items_center().child(
                super::controls::segmented_control(
                    "ref-mode",
                    &[(false, "内置音色"), (true, "麦克风录制")],
                    use_mic,
                    self.keyboard_focus_visible,
                    cx,
                    |this, mic, _, cx| {
                        if this.speech_tools.use_mic_reference != mic {
                            this.speech_tools.use_mic_reference = mic;
                            this.speech_tools.input_changed();
                        }
                        cx.notify();
                    },
                ),
            ));

            if !use_mic {
                let chips = builtin_voices.into_iter().map(|voice| {
                    let active = voice == selected_builtin;
                    let label = voice.strip_suffix(".wav").unwrap_or(&voice).to_owned();
                    let next = voice.clone();
                    div()
                        .id(SharedString::from(format!("builtin-voice-{voice}")))
                        .h(px(30.))
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .rounded_md()
                        .cursor_pointer()
                        .text_xs()
                        .bg(if active {
                            theme_color(ACCENT)
                        } else {
                            theme_color(PANEL)
                        })
                        .border_1()
                        .border_color(if active {
                            theme_color(ACCENT)
                        } else {
                            theme_color(LINE)
                        })
                        .text_color(if active {
                            theme_color(ACCENT_INK)
                        } else {
                            theme_color(TEXT)
                        })
                        .hover(|style| {
                            style.bg(if active {
                                fade(theme_color(ACCENT_HOVER), 1.0)
                            } else {
                                fade(theme_color(CONTROL_TINT), 0.06)
                            })
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.speech_tools.builtin_voice != next {
                                this.speech_tools.input_changed();
                                this.speech_tools.builtin_voice = next.clone();
                                this.speech_tools.builtin_reference = None;
                                this.speech_tools.builtin_error = None;
                                this.load_selected_builtin_voice(cx);
                            }
                            cx.notify();
                        }))
                        .child(icon(
                            "disc",
                            13.,
                            if active {
                                theme_color(ACCENT_INK)
                            } else {
                                theme_color(MUTED)
                            },
                        ))
                        .child(label)
                });
                panel = panel
                    .child(div().flex().flex_wrap().gap_2().children(chips))
                    .when_some(builtin_error, |p, error| p.child(tool_banner(error, true)))
                    .when_some(builtin_ref_path, |p, path| {
                        p.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(self.tiny_button(
                                    "试听参考音色",
                                    "play-builtin-reference",
                                    cx,
                                    move |this, _, cx| this.preview_speech(path.clone(), cx),
                                ))
                                .when_some(builtin_transcript.clone(), |row, _| {
                                    row.child(tool_message("已载入配套逐字稿".into()))
                                }),
                        )
                    })
                    .when_some(builtin_transcript, |p, transcript| {
                        p.child(tool_message(format!("逐字稿：{transcript}")))
                    });
            } else {
                panel = panel
                    .child(tool_message(format!("建议朗读：{RECORDING_PROMPT}")))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .flex_wrap()
                            .items_center()
                            .child(self.hero_button(
                                "record-tts-reference",
                                "audio-lines",
                                if recorded.is_some() {
                                    "重新录制"
                                } else {
                                    "开始录制"
                                },
                                recorded.is_none(),
                                cx,
                                |this, _, cx| this.start_speech_recording(cx),
                            ))
                            .when_some(recorded, |row, path| {
                                row.child(self.tiny_button(
                                    "试听录音",
                                    "play-tts-reference",
                                    cx,
                                    move |this, _, cx| this.preview_speech(path.clone(), cx),
                                ))
                            }),
                    )
                    .child(super::audio_tools::tool_text_input(field));
            }
        }
        panel
    }

    fn confirm_remove_speech_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let model = self.speech_tools.model;
        let root = self.speech_tools.models_root.clone();
        let answer = window.prompt(
            PromptLevel::Warning,
            "移除已安装的语音模型？",
            Some(model.label()),
            &[
                PromptButton::Ok("移除模型".into()),
                PromptButton::Cancel("取消".into()),
            ],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                this.update(cx, |this, cx| {
                    if this.speech_tools.model == model && this.speech_tools.session.is_none() {
                        this.start_speech_job(cx, "正在移除模型文件", move |progress| {
                            progress.ensure_running()?;
                            tts::remove(&root, model)?;
                            Ok(Completion::ModelFiles)
                        });
                    }
                })
                .ok();
            }
        })
        .detach();
    }
}
