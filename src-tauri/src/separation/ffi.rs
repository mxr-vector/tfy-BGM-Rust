//! sherpa-onnx `OfflineSourceSeparation` 的 C API 绑定。
//!
//! Rust 封装尚未暴露该接口，这里直接声明 C 函数并手工管理引擎 / 输出句柄。
//! 全部 `unsafe` 都集中在本文件：输入在 [`with_engine`] 里完成校验，输出缓冲区
//! 只在回调期间借用，返回前由 [`OutputGuard`] 释放。

use std::ffi::{CString, c_char, c_float};

use anyhow::{Context, Result, bail};

use super::Source;

#[repr(C)]
struct SpleeterModelConfig {
    vocals: *const c_char,
    accompaniment: *const c_char,
}

#[repr(C)]
struct UvrModelConfig {
    model: *const c_char,
}

#[repr(C)]
struct SourceSeparationModelConfig {
    spleeter: SpleeterModelConfig,
    uvr: UvrModelConfig,
    num_threads: i32,
    debug: i32,
    provider: *const c_char,
}

#[repr(C)]
struct SourceSeparationConfig {
    model: SourceSeparationModelConfig,
}

#[repr(C)]
struct OfflineSourceSeparation {
    _private: [u8; 0],
}

#[repr(C)]
struct SourceSeparationStem {
    samples: *const *const c_float,
    num_channels: i32,
    n: i32,
}

#[repr(C)]
struct SourceSeparationOutput {
    stems: *const SourceSeparationStem,
    num_stems: i32,
    sample_rate: i32,
}

unsafe extern "C" {
    fn SherpaOnnxCreateOfflineSourceSeparation(
        config: *const SourceSeparationConfig,
    ) -> *const OfflineSourceSeparation;
    fn SherpaOnnxDestroyOfflineSourceSeparation(ss: *const OfflineSourceSeparation);
    fn SherpaOnnxOfflineSourceSeparationProcess(
        ss: *const OfflineSourceSeparation,
        samples: *const *const c_float,
        num_channels: i32,
        num_samples: i32,
        sample_rate: i32,
    ) -> *const SourceSeparationOutput;
    fn SherpaOnnxDestroySourceSeparationOutput(p: *const SourceSeparationOutput);
}

struct Engine(*const OfflineSourceSeparation);

impl Drop for Engine {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: 句柄只由本模块创建，且 Drop 只执行一次。
            unsafe { SherpaOnnxDestroyOfflineSourceSeparation(self.0) };
        }
    }
}

struct OutputGuard(*const SourceSeparationOutput);

impl Drop for OutputGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: 输出只由 sherpa-onnx 的 Process 分配，且只释放一次。
            unsafe { SherpaOnnxDestroySourceSeparationOutput(self.0) };
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(super) struct Stems {
    pub(super) sample_rate: u32,
    pub(super) stems: Vec<Vec<Vec<f32>>>,
}

/// 引擎输出；声道切片借用 sherpa-onnx 的缓冲区，仅在 `with_engine` 的回调内有效。
pub(super) struct RawStems<'a> {
    pub(super) sample_rate: u32,
    pub(super) stems: Vec<Vec<&'a [f32]>>,
}

/// 调用 sherpa-onnx 引擎并把结果交给回调处理。
///
/// 回调直接消费借用切片，避免为整首歌的人声 / 伴奏再复制一整份内存；
/// 返回前引擎缓冲区由 `OutputGuard` 释放。
pub(super) fn with_engine<R>(
    source: &Source,
    channels: &[Vec<f32>],
    sample_rate: u32,
    num_threads: i32,
    consume: impl FnOnce(RawStems<'_>) -> Result<R>,
) -> Result<R> {
    if channels.is_empty() {
        bail!("分离输入没有音频声道");
    }
    let n = channels[0].len();
    if n == 0 {
        bail!("分离输入音频为空");
    }
    if channels.iter().any(|channel| channel.len() != n) {
        bail!("分离输入各声道长度不一致");
    }

    let empty = CString::new("").context("空字符串转换失败")?;
    let provider = CString::new("cpu").context("引擎提供者转换失败")?;
    let vocals_path;
    let accompaniment_path;
    let uvr_model;
    let config = match source {
        Source::Spleeter {
            vocals: vocals_file,
            accompaniment,
        } => {
            vocals_path = CString::new(vocals_file.to_string_lossy().as_bytes())
                .context("模型路径包含 NUL 字符")?;
            accompaniment_path = CString::new(accompaniment.to_string_lossy().as_bytes())
                .context("模型路径包含 NUL 字符")?;
            uvr_model = empty.clone();
            SourceSeparationConfig {
                model: SourceSeparationModelConfig {
                    spleeter: SpleeterModelConfig {
                        vocals: vocals_path.as_ptr(),
                        accompaniment: accompaniment_path.as_ptr(),
                    },
                    uvr: UvrModelConfig {
                        model: uvr_model.as_ptr(),
                    },
                    num_threads,
                    debug: 0,
                    provider: provider.as_ptr(),
                },
            }
        }
        Source::Uvr { model } => {
            vocals_path = empty.clone();
            accompaniment_path = empty.clone();
            uvr_model = CString::new(model.to_string_lossy().as_bytes())
                .context("模型路径包含 NUL 字符")?;
            SourceSeparationConfig {
                model: SourceSeparationModelConfig {
                    spleeter: SpleeterModelConfig {
                        vocals: vocals_path.as_ptr(),
                        accompaniment: accompaniment_path.as_ptr(),
                    },
                    uvr: UvrModelConfig {
                        model: uvr_model.as_ptr(),
                    },
                    num_threads,
                    debug: 0,
                    provider: provider.as_ptr(),
                },
            }
        }
    };

    // SAFETY: config 内的指针指向本函数持有的 CString，调用期间保持有效。
    let engine = unsafe { SherpaOnnxCreateOfflineSourceSeparation(&config) };
    if engine.is_null() {
        bail!(
            "无法创建人声分离引擎（模型需为 sherpa-onnx 兼容的 Spleeter/UVR 模型；当前模型不兼容时引擎会拒绝加载）"
        );
    }
    let engine = Engine(engine);

    let pointers: Vec<*const c_float> = channels.iter().map(|channel| channel.as_ptr()).collect();
    // SAFETY: 声道指针与样本数均由上面的校验保证有效；输出由 guard 释放。
    let output = unsafe {
        SherpaOnnxOfflineSourceSeparationProcess(
            engine.0,
            pointers.as_ptr(),
            channels.len() as i32,
            n as i32,
            sample_rate as i32,
        )
    };
    if output.is_null() {
        bail!("人声分离失败：sherpa-onnx 未返回结果");
    }
    let guard = OutputGuard(output);
    // SAFETY: output 非空且由 sherpa-onnx 填充；在 guard 生命周期内只读。
    let data = unsafe { &*guard.0 };
    if data.stems.is_null() {
        bail!("人声分离失败：模型未返回音轨数据");
    }
    if data.num_stems < 2 {
        bail!("人声分离失败：模型只输出 {} 个音轨", data.num_stems);
    }
    if data.sample_rate <= 0 {
        bail!("人声分离失败：输出采样率无效");
    }

    let mut stems = Vec::with_capacity(data.num_stems as usize);
    for index in 0..data.num_stems as usize {
        // SAFETY: stems 数组长度为 num_stems，索引在范围内。
        let stem = unsafe { &*data.stems.add(index) };
        // SAFETY: stem 由 sherpa-onnx 填充，输出缓冲区在 guard 释放前保持有效。
        let channels = unsafe { read_stem(stem, index) }?;
        stems.push(channels);
    }

    consume(RawStems {
        sample_rate: data.sample_rate as u32,
        stems,
    })
}

/// 把一条 stem 的声道指针转成借用切片；调用方保证输出缓冲区在切片使用期间有效。
///
/// # Safety
///
/// `stem` 必须由 sherpa-onnx 填充，且对应的输出缓冲区在返回的切片使用期间不被释放。
unsafe fn read_stem(stem: &SourceSeparationStem, index: usize) -> Result<Vec<&[f32]>> {
    if stem.n <= 0 || stem.num_channels <= 0 {
        bail!("人声分离失败：第 {} 个音轨为空", index + 1);
    }
    let mut channels = Vec::with_capacity(stem.num_channels as usize);
    for channel in 0..stem.num_channels as usize {
        // SAFETY: samples 数组长度为 num_channels，n 为每声道样本数。
        let pointer = unsafe { *stem.samples.add(channel) };
        if pointer.is_null() {
            bail!("人声分离失败：第 {} 个音轨缺少声道数据", index + 1);
        }
        // SAFETY: 指针由上面的空值检查确认，长度为 stem.n，缓冲区生命周期由调用方保证。
        channels.push(unsafe { std::slice::from_raw_parts(pointer, stem.n as usize) });
    }
    Ok(channels)
}

/// 调用引擎并复制出独立音轨；仅测试与需要持有数据的调用方使用。
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn run_engine(
    source: &Source,
    channels: &[Vec<f32>],
    sample_rate: u32,
    num_threads: i32,
) -> Result<Stems> {
    with_engine(source, channels, sample_rate, num_threads, |raw| {
        let stems = raw
            .stems
            .into_iter()
            .map(|channels| channels.into_iter().map(<[f32]>::to_vec).collect())
            .collect();
        Ok(Stems {
            sample_rate: raw.sample_rate,
            stems,
        })
    })
}
