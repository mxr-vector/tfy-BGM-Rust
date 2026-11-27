use std::io::{Read, Seek};
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use tokio_util::sync::CancellationToken;

use crate::tts::check_cancelled;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NvidiaDevice {
    pub uuid: String,
    pub free_mib: u64,
}

fn select_device(
    stdout: &str,
    minimum_mib: u64,
    mask: Option<&str>,
) -> Result<Option<NvidiaDevice>> {
    let mut devices = Vec::new();
    for row in stdout.lines().map(str::trim).filter(|row| !row.is_empty()) {
        let (uuid, memory) = row.split_once(',').context("显卡 UUID 或空闲显存缺失")?;
        let uuid = uuid.trim();
        let suffix = uuid.strip_prefix("GPU-").context("显卡 UUID 格式无效")?;
        ensure!(
            suffix.len() == 36
                && suffix.bytes().enumerate().all(|(index, byte)| {
                    if matches!(index, 8 | 13 | 18 | 23) {
                        byte == b'-'
                    } else {
                        byte.is_ascii_hexdigit()
                    }
                }),
            "显卡 UUID 格式无效"
        );
        if let Ok(free_mib) = memory.trim().parse::<u64>() {
            devices.push(NvidiaDevice {
                uuid: uuid.to_owned(),
                free_mib,
            });
        }
    }
    let single_device = stdout.lines().filter(|row| !row.trim().is_empty()).count() == 1;
    Ok(devices
        .into_iter()
        .filter(|device| {
            device.free_mib >= minimum_mib
                && mask.is_none_or(|mask| {
                    mask.split(',')
                        .map(str::trim)
                        .take_while(|value| !value.is_empty() && *value != "-1")
                        .any(|value| value == device.uuid || (single_device && value == "0"))
                })
        })
        .max_by_key(|device| device.free_mib))
}

pub fn force_cpu() -> bool {
    match std::env::var("TFY_SPEECH_DEVICE") {
        Ok(value) => !value.trim().eq_ignore_ascii_case("auto"),
        Err(std::env::VarError::NotPresent) => false,
        Err(_) => true,
    }
}

pub fn minimum_vram_mib(variable: &str, default: u64) -> u64 {
    std::env::var(variable)
        .ok()
        .or_else(|| {
            if variable == "TFY_TTS_MIN_VRAM_MIB" {
                std::env::var("TFY_COSYVOICE_MIN_VRAM_MIB").ok()
            } else {
                None
            }
        })
        .and_then(|value| value.trim().parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

pub fn eligible_nvidia_device(
    minimum_mib: u64,
    token: &CancellationToken,
) -> Result<Option<NvidiaDevice>> {
    check_cancelled(token)?;
    if force_cpu() {
        return Ok(None);
    }
    let mask = match std::env::var("CUDA_VISIBLE_DEVICES") {
        Ok(mask) if mask.trim().is_empty() || mask.trim() == "-1" => return Ok(None),
        Ok(mask) => Some(mask),
        Err(std::env::VarError::NotPresent) => None,
        Err(error) => return Err(error).context("无法读取 CUDA_VISIBLE_DEVICES"),
    };
    let mut output = tempfile::tempfile().context("无法创建 GPU 检测输出文件")?;
    let mut child = bgm_core::process::command(Path::new("nvidia-smi"))
        .args([
            "--query-gpu=uuid,memory.free",
            "--format=csv,noheader,nounits",
        ])
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(Stdio::null())
        .spawn()
        .context("无法启动 nvidia-smi")?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if token.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            check_cancelled(token)?;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                ensure!(status.success(), "nvidia-smi 返回错误状态");
                break;
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                result.context("无法检查 nvidia-smi")?;
                bail!("nvidia-smi 查询超时");
            }
        }
    }
    output.rewind()?;
    let mut text = String::new();
    output.take(16 * 1024 + 1).read_to_string(&mut text)?;
    ensure!(text.len() <= 16 * 1024, "nvidia-smi 输出过大");
    select_device(&text, minimum_mib, mask.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_selects_visible_devices_with_enough_free_memory() {
        let first = "GPU-00000000-0000-0000-0000-000000000001";
        let second = "GPU-00000000-0000-0000-0000-000000000002";
        let rows = format!("{first}, 3000\r\n{second}, 8000\r\n");
        assert_eq!(
            select_device(&rows, 4000, None).unwrap().unwrap().uuid,
            second
        );
        assert_eq!(
            select_device(&rows, 2048, Some(first))
                .unwrap()
                .unwrap()
                .uuid,
            first
        );
        assert!(select_device(&rows, 4000, Some(first)).unwrap().is_none());
        assert!(select_device(&rows, 8192, None).unwrap().is_none());
        assert!(select_device(&rows, 4000, Some("0")).unwrap().is_none());
        assert!(select_device("invalid", 4000, None).is_err());
        assert!(
            select_device(&format!("{first}, [N/A]"), 4000, None)
                .unwrap()
                .is_none()
        );
    }
}
