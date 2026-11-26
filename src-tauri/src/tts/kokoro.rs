use anyhow::{Result, bail};
use sherpa_onnx::{GenerationConfig, OfflineTts, OfflineTtsConfig};

use crate::tts_models::TtsModel;

pub fn configure(config: &mut OfflineTtsConfig, model: &TtsModel) {
    let path = std::path::Path::new(&model.path);
    let join = |file: &str| path.join(file).to_string_lossy().into_owned();
    config.model.kokoro.model = Some(join(model.acoustic_file()));
    config.model.kokoro.voices = Some(join("voices.bin"));
    config.model.kokoro.tokens = Some(join("tokens.txt"));
    config.model.kokoro.lexicon = Some(format!(
        "{},{}",
        join("lexicon-us-en.txt"),
        join("lexicon-zh.txt")
    ));
}

pub fn generation(voice_id: i32, engine: &OfflineTts) -> Result<GenerationConfig> {
    if !(0..engine.num_speakers()).contains(&voice_id) {
        bail!("所选 Kokoro 语音不可用");
    }
    Ok(GenerationConfig {
        sid: voice_id,
        silence_scale: 0.3,
        ..GenerationConfig::default()
    })
}
