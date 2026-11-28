//! Microsoft Edge 在线朗读（edge-tts）。
//!
//! 与靠本地模型推理的 ZipVoice / CosyVoice3 不同，这个引擎不读本地模型：
//! 文本通过 WebSocket 发给 Edge 的朗读服务，返回 24 kHz 单声道 MP3，
//! 再由纯 Rust 的 symphonia 解码成 PCM 供统一的轨道拼装流程使用。

use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use edge_tts_rust::{Boundary, EdgeTtsClient, SpeakOptions};
use serde::Serialize;
use symphonia_bundle_mp3::{MpaDecoder, MpaReader};
use symphonia_core::{
    audio::SampleBuffer,
    codecs::{CODEC_TYPE_NULL, Decoder, DecoderOptions},
    errors::Error as SymphoniaError,
    formats::{FormatOptions, FormatReader},
    io::{MediaSourceStream, MediaSourceStreamOptions},
};
use tokio_util::sync::CancellationToken;

/// Edge 朗读服务固定输出 24 kHz，与轨道拼装使用的采样率保持一致。
pub const SAMPLE_RATE: u32 = 24_000;
/// 只有 MP3 与 Opus 两种可用格式；MP3 便于用纯 Rust 解码。
const OUTPUT_FORMAT: &str = "audio-24khz-48kbitrate-mono-mp3";
/// 基准测试与默认可选音色。
pub const DEFAULT_VOICE: &str = "zh-CN-XiaoxiaoNeural";
/// 语速允许的百分比区间，避免合成请求被服务端拒绝。
const RATE_LIMIT: f32 = 100.0;
/// 在线音色列表缓存：成功结果保存 12 小时，离线兜底清单只保存 10 分钟。
const VOICE_CACHE_TTL: Duration = Duration::from_secs(12 * 60 * 60);
const FALLBACK_CACHE_TTL: Duration = Duration::from_secs(10 * 60);

/// 服务不可达时使用的常见音色，保证界面仍可正常展示与选择。
const FALLBACK_VOICES: &[(&str, &str)] = &[
    ("zh-CN-XiaoxiaoNeural", "Female"),
    ("zh-CN-XiaoyiNeural", "Female"),
    ("zh-CN-YunjianNeural", "Male"),
    ("zh-CN-YunxiNeural", "Male"),
    ("zh-CN-YunxiaNeural", "Male"),
    ("zh-CN-YunyangNeural", "Male"),
    ("zh-CN-liaoning-XiaobeiNeural", "Female"),
    ("zh-CN-shaanxi-XiaoniNeural", "Female"),
    ("zh-HK-HiuGaaiNeural", "Female"),
    ("zh-HK-HiuMaanNeural", "Female"),
    ("zh-HK-WanLungNeural", "Male"),
    ("zh-TW-HsiaoChenNeural", "Female"),
    ("zh-TW-HsiaoYuNeural", "Female"),
    ("zh-TW-YunJheNeural", "Male"),
    ("en-US-AriaNeural", "Female"),
    ("en-US-GuyNeural", "Male"),
    ("en-US-JennyNeural", "Female"),
    ("en-US-EmmaMultilingualNeural", "Female"),
];

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeVoice {
    /// 服务端短名，合成请求直接使用。
    pub name: String,
    pub locale: String,
    pub gender: String,
    pub friendly_name: String,
}

struct VoiceCache {
    at: Instant,
    live: bool,
    voices: Vec<EdgeVoice>,
}

static VOICES: parking_lot::RwLock<Option<VoiceCache>> = parking_lot::RwLock::new(None);

/// 读取可用音色；优先命中缓存，网络失败时退回内置清单。
pub fn voices() -> Result<Vec<EdgeVoice>> {
    if let Some(cache) = VOICES.read().as_ref()
        && cache.at.elapsed() < if cache.live { VOICE_CACHE_TTL } else { FALLBACK_CACHE_TTL }
    {
        return Ok(cache.voices.clone());
    }
    let client = EdgeTtsClient::builder()
        .ws_warmup(false)
        .connect_timeout(Duration::from_secs(15))
        .receive_timeout(Duration::from_secs(60))
        .build()
        .context("无法初始化 Edge 在线朗读客户端")?;
    install_crypto_provider();
    let fetched = tauri::async_runtime::block_on(client.list_voices());
    let (live, voices) = match fetched {
        Ok(voices) => (true, collect(voices)),
        Err(error) => {
            if let Some(cache) = VOICES.read().as_ref() {
                return Ok(cache.voices.clone());
            }
            eprintln!("[tts] 无法获取 Edge 音色列表，使用内置清单：{error}");
            (false, fallback())
        }
    };
    ensure!(!voices.is_empty(), "Edge 没有返回可用音色");
    *VOICES.write() = Some(VoiceCache {
        at: Instant::now(),
        live,
        voices: voices.clone(),
    });
    Ok(voices)
}

fn collect(mut voices: Vec<edge_tts_rust::Voice>) -> Vec<EdgeVoice> {
    voices.retain(|voice| {
        !voice.short_name.is_empty()
            && voice.status.as_deref().is_none_or(|status| status == "GA")
    });
    let mut voices: Vec<EdgeVoice> = voices
        .into_iter()
        .map(|voice| EdgeVoice {
            name: voice.short_name,
            locale: voice.locale,
            gender: voice.gender,
            friendly_name: voice.friendly_name.unwrap_or_default(),
        })
        .collect();
    voices.sort_by(|left, right| {
        locale_rank(&left.locale)
            .cmp(&locale_rank(&right.locale))
            .then_with(|| left.locale.cmp(&right.locale))
            .then_with(|| left.name.cmp(&right.name))
    });
    voices.dedup_by(|left, right| left.name == right.name);
    voices
}

/// 中文优先，其次英文，其余按语言代码排序。
fn locale_rank(locale: &str) -> u8 {
    if locale.starts_with("zh") {
        0
    } else if locale.starts_with("en") {
        1
    } else {
        2
    }
}

fn fallback() -> Vec<EdgeVoice> {
    FALLBACK_VOICES
        .iter()
        .map(|(name, gender)| EdgeVoice {
            name: (*name).to_owned(),
            locale: name.split('-').take(2).collect::<Vec<_>>().join("-"),
            gender: (*gender).to_owned(),
            friendly_name: String::new(),
        })
        .collect()
}

pub struct Engine {
    client: EdgeTtsClient,
}

/// 进程内只安装一次加密后端：依赖树里 ring 与 aws-lc-rs 同时存在，
/// rustls 无法自动决定默认 provider，WebSocket 建连会直接 panic。
fn install_crypto_provider() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        }
    });
}

impl Engine {
    pub fn load() -> Result<Self> {
        install_crypto_provider();
        let client = EdgeTtsClient::builder()
            // 预热会 tokio::spawn，而构造发生在阻塞任务里；按需建连更稳妥。
            .ws_warmup(false)
            .ws_pool_size(1)
            .connect_timeout(std::time::Duration::from_secs(15))
            .receive_timeout(std::time::Duration::from_secs(120))
            .build()
            .context("无法初始化 Edge 在线朗读客户端")?;
        Ok(Self { client })
    }

    pub fn device(&self) -> &'static str {
        "cloud"
    }

    /// 在线客户端没有需要释放的本地资源。
    pub fn is_alive(&self) -> bool {
        true
    }

    pub fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    pub fn generate(
        &self,
        text: &str,
        voice: &str,
        speed: f32,
        token: &CancellationToken,
    ) -> Result<Vec<f32>> {
        let text = text.trim();
        ensure!(!text.is_empty(), "合成文本为空");
        let options = SpeakOptions {
            voice: validate_voice(voice)?,
            rate: rate_value(speed),
            output_format: OUTPUT_FORMAT.to_owned(),
            boundary: Boundary::Sentence,
            ..SpeakOptions::default()
        };
        let outcome = tauri::async_runtime::block_on(
            token.run_until_cancelled(self.client.synthesize(text, options)),
        )
        .context("语音任务已取消")?;
        let result = outcome.map_err(|error| anyhow::anyhow!("Edge 在线朗读失败：{error}"))?;
        let samples = decode_mp3(&result.audio)?;
        ensure!(!samples.is_empty(), "Edge 在线朗读没有返回音频");
        Ok(samples)
    }
}

/// 音色名会被拼进 SSML，只接受服务端的 `语言-区域-名称` 短名。
pub fn validate_voice(voice: &str) -> Result<String> {
    let voice = voice.trim();
    ensure!(!voice.is_empty(), "请选择 Edge 朗读音色");
    ensure!(
        voice.len() <= 96
            && voice.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            && voice.split('-').count() >= 3,
        "Edge 朗读音色名称无效"
    );
    Ok(voice.to_owned())
}

/// 把倍速换算成 Edge 的 `+N%` 语速参数。
fn rate_value(speed: f32) -> String {
    let speed = if speed.is_finite() { speed } else { 1.0 };
    let percent = ((speed.clamp(0.5, 2.0) - 1.0) * 100.0).round() as i32;
    let percent = percent.clamp(-(RATE_LIMIT as i32), RATE_LIMIT as i32);
    format!("{percent:+}%")
}

/// 解码 Edge 返回的 MP3；多段文本会拼成连续帧，一次解码即可。
fn decode_mp3(bytes: &[u8]) -> Result<Vec<f32>> {
    ensure!(!bytes.is_empty(), "Edge 在线朗读没有返回音频");
    let source = MediaSourceStream::new(
        Box::new(std::io::Cursor::new(bytes.to_vec())),
        MediaSourceStreamOptions::default(),
    );
    let mut reader =
        MpaReader::try_new(source, &FormatOptions::default()).context("无法解析 Edge 返回的 MP3")?;
    let track = reader
        .tracks()
        .iter()
        .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
        .context("Edge 返回的 MP3 缺少音频轨道")?;
    let track_id = track.id;
    ensure!(
        track
            .codec_params
            .sample_rate
            .is_none_or(|rate| rate == SAMPLE_RATE),
        "Edge 返回的采样率不是 24 kHz"
    );
    let mut decoder = MpaDecoder::try_new(&track.codec_params, &DecoderOptions::default())
        .context("无法创建 MP3 解码器")?;
    let mut samples = Vec::new();
    let mut buffer: Option<(SampleBuffer<f32>, symphonia_core::audio::SignalSpec)> = None;
    loop {
        let packet = match reader.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(error) => return Err(error).context("读取 MP3 帧失败"),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(audio) => {
                let spec = *audio.spec();
                if buffer.as_ref().is_none_or(|(_, current)| *current != spec) {
                    buffer = Some((
                        SampleBuffer::<f32>::new(audio.capacity() as u64, spec),
                        spec,
                    ));
                }
                let (buffer, _) = buffer.as_mut().context("MP3 解码缓冲区不可用")?;
                buffer.copy_interleaved_ref(audio);
                let channels = spec.channels.count();
                if channels <= 1 {
                    samples.extend_from_slice(buffer.samples());
                } else {
                    samples.extend(buffer.samples().chunks(channels).map(|frame| {
                        frame.iter().copied().sum::<f32>() / channels as f32
                    }));
                }
            }
            // 个别帧损坏时跳过，避免整段朗读失败。
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(error) => return Err(error).context("解码 MP3 失败"),
        }
    }
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_is_rendered_as_a_signed_percentage() {
        assert_eq!(rate_value(1.0), "+0%");
        assert_eq!(rate_value(1.25), "+25%");
        assert_eq!(rate_value(0.9), "-10%");
        // 超出范围与非法输入都被收敛到安全值。
        assert_eq!(rate_value(4.0), "+100%");
        assert_eq!(rate_value(0.1), "-50%");
        assert_eq!(rate_value(f32::NAN), "+0%");
    }

    #[test]
    fn voice_names_are_restricted_to_short_names() {
        assert_eq!(
            validate_voice(" zh-CN-XiaoxiaoNeural ").unwrap(),
            "zh-CN-XiaoxiaoNeural"
        );
        for bad in [
            "",
            "zh-CN",
            "zh-CN-XiaoxiaoNeural'/><voice name='x",
            "zh CN Xiao",
        ] {
            assert!(validate_voice(bad).is_err(), "{bad} 不应通过");
        }
    }

    #[test]
    fn empty_payload_is_rejected() {
        assert!(decode_mp3(&[]).is_err());
        assert!(decode_mp3(b"not mp3").is_err());
    }

    /// 联网验收：真实请求一次 Edge 朗读，确认 MP3 能解成 24 kHz PCM。
    #[test]
    #[ignore = "需要联网访问 Edge 朗读服务"]
    fn live_synthesis_returns_24khz_audio() {
        let engine = Engine::load().unwrap();
        let token = CancellationToken::new();
        let samples = engine
            .generate("这是一次在线合成长度验收。", DEFAULT_VOICE, 1.0, &token)
            .unwrap();
        assert!(samples.len() > SAMPLE_RATE as usize / 4, "音频过短");
        assert!(samples.iter().all(|sample| sample.is_finite()));
        let peak = samples.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()));
        assert!(peak > 0.01, "音频接近静音：peak={peak}");
    }

    /// 联网验收：音色列表应包含中文音色。
    #[test]
    #[ignore = "需要联网访问 Edge 朗读服务"]
    fn live_voice_list_contains_chinese_voices() {
        let voices = voices().unwrap();
        assert!(voices.len() > 20, "音色数量异常：{}", voices.len());
        assert!(
            voices
                .iter()
                .any(|voice| voice.name == "zh-CN-XiaoxiaoNeural"),
            "缺少 zh-CN-XiaoxiaoNeural"
        );
    }
}
