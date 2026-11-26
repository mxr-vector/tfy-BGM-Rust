use sherpa_onnx::{GenerationConfig, OfflineTtsConfig};

use crate::{speech_audio, tts_models::TtsModel};

pub fn configure(config: &mut OfflineTtsConfig, root: &std::path::Path, model: &TtsModel) {
    let path = std::path::Path::new(&model.path);
    let join = |file: &str| path.join(file).to_string_lossy().into_owned();
    let (encoder, decoder) = model.zipvoice_files();
    let deps = root.join(crate::tts_models::DIR);
    config.model.zipvoice.tokens = Some(join("tokens.txt"));
    config.model.zipvoice.encoder = Some(join(encoder));
    config.model.zipvoice.decoder = Some(join(decoder));
    config.model.zipvoice.vocoder =
        Some(deps.join("vocos_24khz.onnx").to_string_lossy().into_owned());
    config.model.zipvoice.lexicon = Some(format!(
        "{},{}",
        join("lexicon.txt"),
        deps.join("lexicon-en-us.txt").display()
    ));
    config.model.zipvoice.feat_scale = 0.1;
    config.model.zipvoice.t_shift = 0.5;
    config.model.zipvoice.target_rms = 0.1;
    config.model.zipvoice.guidance_scale = 1.0;
}

pub fn generation(reference_audio: Vec<f32>, reference_text: &str) -> GenerationConfig {
    GenerationConfig {
        reference_text: Some(reference_text.to_owned()),
        reference_audio: Some(reference_audio),
        reference_sample_rate: speech_audio::REFERENCE_RATE as i32,
        num_steps: 4,
        silence_scale: 0.3,
        ..GenerationConfig::default()
    }
}
