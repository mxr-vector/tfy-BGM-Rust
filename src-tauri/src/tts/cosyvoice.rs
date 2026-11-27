use std::{
    env, fs,
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::check_cancelled;
use crate::{
    speech_audio,
    speech_device::{eligible_nvidia_device, force_cpu, minimum_vram_mib},
};
use sha2::Digest;

const SAMPLE_RATE: u32 = 24_000;
const LOAD_TIMEOUT: Duration = Duration::from_secs(600);
/// 首次环境自检会加载整套 ONNX 模型，慢盘或纯 CPU 机器需要更长时间。
const ENV_CHECK_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const SETUP_COMMAND_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const GENERATE_TIMEOUT: Duration = Duration::from_secs(300);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const STDERR_LIMIT: usize = 16 * 1024;
const FRAME_LIMIT: u64 = 16 * 1024;
const FRAME_PREFIX: &[u8] = b"TFY_COSYVOICE ";
static RUNTIME_SETUP: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
/// 运行环境布局变化时递增，使旧缓存环境只迁移安装一次。
const COSYVOICE_ENV: &str = "v2";
/// 低于该显存量直接使用 CPU，避免加载 0.5B 权重时显存不足。
const MIN_GPU_MIB: u64 = 6_144;

// Lourdle/CosyVoice 24c9212eb54e832af4d9b03bcd9157d88acbf6db.
const PYTHON_WORKER: &str = include_str!("../py/cosyvoice_worker.py");

fn runtime_source(
    app_cache: &Path,
    token: &CancellationToken,
    progress: &dyn Fn(&str),
) -> Result<PathBuf> {
    progress("正在检查 CosyVoice3 运行环境…");
    // 代码包与环境都只允许准备一次，避免并发解压/安装互相覆盖。
    let _setup = RUNTIME_SETUP.lock();
    let home = runtime_home(app_cache);
    if !runtime_ready(&home) {
        if home.exists() {
            fs::remove_dir_all(&home).context("无法清理不完整的 CosyVoice3 运行代码")?;
        }
        prepare_runtime(&home, token, progress)?;
    }
    Ok(home)
}

fn runtime_home(app_cache: &Path) -> PathBuf {
    app_cache.join("cosyvoice/runtime/Lourdle-CosyVoice-24c9212")
}

fn runtime_ready(home: &Path) -> bool {
    home.join("cosyvoice/cli/cosyvoice.py").is_file()
        && home
            .join("third_party/Matcha-TTS/matcha/utils/audio.py")
            .is_file()
}

fn runtime_python(app_cache: &Path) -> PathBuf {
    let binary = if cfg!(windows) {
        "Scripts/python.exe"
    } else {
        "bin/python3"
    };
    app_cache.join("cosyvoice/venv").join(binary)
}

fn managed_cuda_dir(app_cache: &Path) -> PathBuf {
    app_cache.join(format!("cosyvoice/venv-cuda-{COSYVOICE_ENV}"))
}

fn managed_cuda_python(app_cache: &Path) -> PathBuf {
    managed_cuda_dir(app_cache).join("Scripts/python.exe")
}

fn managed_cuda_marker(app_cache: &Path) -> PathBuf {
    managed_cuda_dir(app_cache).join(".tfy-bgm-cuda-ready")
}

pub(super) fn managed_cuda_status(app_cache: &Path) -> Option<bool> {
    cfg!(all(windows, target_arch = "x86_64")).then(|| {
        managed_cuda_python(app_cache).is_file()
            && fs::read_to_string(managed_cuda_marker(app_cache))
                .is_ok_and(|value| value == "ready\n")
    })
}

fn managed_cuda_cache(app_cache: &Path) -> PathBuf {
    app_cache.join("cosyvoice/uv-cache-cuda")
}

pub(super) fn managed_cuda_present(app_cache: &Path) -> bool {
    cfg!(all(windows, target_arch = "x86_64"))
        && [managed_cuda_dir(app_cache), managed_cuda_cache(app_cache)]
            .iter()
            .any(|path| path.symlink_metadata().is_ok())
}

pub(super) fn remove_managed_cuda(app_cache: &Path) -> Result<()> {
    ensure!(
        cfg!(all(windows, target_arch = "x86_64")),
        "当前平台不支持托管 GPU 环境"
    );
    let _setup = RUNTIME_SETUP.lock();
    let cache = app_cache.canonicalize()?;
    let paths = [managed_cuda_dir(&cache), managed_cuda_cache(&cache)];
    for path in &paths {
        match path.canonicalize() {
            Ok(target) => ensure!(&target == path, "GPU 环境目录指向链接，无法删除"),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                ensure!(
                    path.symlink_metadata().is_err(),
                    "GPU 环境目录指向无效链接，无法删除"
                );
            }
            Err(error) => return Err(error).context("无法定位 GPU 环境目录"),
        }
    }
    for path in paths {
        match fs::remove_dir_all(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("无法删除 GPU 环境目录"),
        }
    }
    Ok(())
}

fn selected_cuda_python(app_cache: &Path) -> Option<PathBuf> {
    cuda_python(env::var_os("TFY_COSYVOICE_CUDA_PYTHON")).or_else(|| {
        (managed_cuda_status(app_cache) == Some(true)).then(|| managed_cuda_python(app_cache))
    })
}

/// uv 管理的 Python 安装目录名由 `uv python install` 决定，格式为
/// `cpython-<版本>-<系统>-<架构>-<libc>`（仅 Linux 区分 gnu/musl）。
fn managed_python(root: &Path) -> PathBuf {
    let os = std::env::consts::OS;
    let environment = if os == "linux" {
        if cfg!(target_env = "musl") {
            "musl"
        } else {
            "gnu"
        }
    } else {
        "none"
    };
    let directory = format!(
        "cpython-3.11.14-{os}-{}-{environment}",
        std::env::consts::ARCH
    );
    let binary = if cfg!(windows) {
        "python.exe"
    } else {
        "bin/python3"
    };
    root.join("python").join(directory).join(binary)
}

#[derive(Debug, PartialEq, Eq)]
struct UvArtifact {
    url: &'static str,
    sha256: &'static str,
    executable: &'static str,
}

fn uv_artifact(os: &str, arch: &str, environment: &str) -> Result<UvArtifact> {
    let artifact = match (os, arch, environment) {
        ("windows", "x86_64", _) => UvArtifact {
            url: "https://files.pythonhosted.org/packages/07/5d/eb80c6eff2a9f7d5cf35ec84fda323b74aa0054145db28baf72d35a7a301/uv-0.9.26-py3-none-win_amd64.whl",
            sha256: "0315fc321f5644b12118f9928086513363ed9b29d74d99f1539fda1b6b5478ab",
            executable: "uv.exe",
        },
        ("windows", "aarch64", _) => UvArtifact {
            url: "https://files.pythonhosted.org/packages/ed/9d/3b2631931649b1783f5024796ca8ad2b42a01a829b9ce1202d973cc7bce5/uv-0.9.26-py3-none-win_arm64.whl",
            sha256: "344ff38749b6cd7b7dfdfb382536f168cafe917ae3a5aa78b7a63746ba2a905b",
            executable: "uv.exe",
        },
        ("macos", "x86_64", _) => UvArtifact {
            url: "https://files.pythonhosted.org/packages/a5/8b/68ac5825a615a8697e324f52ac0b92feb47a0ec36a63759c5f2931f0c3a0/uv-0.9.26-py3-none-macosx_10_12_x86_64.whl",
            sha256: "b815e3b26eeed00e00f831343daba7a9d99c1506883c189453bb4d215f54faac",
            executable: "uv",
        },
        ("macos", "aarch64", _) => UvArtifact {
            url: "https://files.pythonhosted.org/packages/0d/a2/664a338aefe009f6e38e47455ee2f64a21da7ad431dbcaf8b45d8b1a2b7a/uv-0.9.26-py3-none-macosx_11_0_arm64.whl",
            sha256: "1b012e6c4dfe767f818cbb6f47d02c207c9b0c82fee69a5de6d26ffb26a3ef3c",
            executable: "uv",
        },
        ("linux", "x86_64", "gnu") => UvArtifact {
            url: "https://files.pythonhosted.org/packages/38/16/a07593a040fe6403c36f3b0a99b309f295cbfe19a1074dbadb671d5d4ef7/uv-0.9.26-py3-none-manylinux_2_17_x86_64.manylinux2014_x86_64.whl",
            sha256: "b7e89798bd3df7dcc4b2b4ac4e2fc11d6b3ff4fe7d764aa3012d664c635e2922",
            executable: "uv",
        },
        ("linux", "x86_64", "musl") => UvArtifact {
            url: "https://files.pythonhosted.org/packages/6b/e2/be683e30262f2cf02dcb41b6c32910a6939517d50ec45f502614d239feb7/uv-0.9.26-py3-none-musllinux_1_1_x86_64.whl",
            sha256: "25278f9298aa4dade38241a93d036739b0c87278dcfad1ec1f57e803536bfc49",
            executable: "uv",
        },
        ("linux", "aarch64", "gnu" | "musl") => UvArtifact {
            url: "https://files.pythonhosted.org/packages/ba/3d/b8186a7dec1346ca4630c674b760517d28bffa813a01965f4b57596bacf3/uv-0.9.26-py3-none-manylinux_2_17_aarch64.manylinux2014_aarch64.musllinux_1_1_aarch64.whl",
            sha256: "ea296b700d7c4c27acdfd23ffaef2b0ecdd0aa1b58d942c62ee87df3b30f06ac",
            executable: "uv",
        },
        _ => bail!("当前平台暂不支持自动准备 CosyVoice3 的 uv：{os}-{arch}-{environment}"),
    };
    Ok(artifact)
}

fn target_environment() -> &'static str {
    #[cfg(target_env = "musl")]
    {
        "musl"
    }
    #[cfg(target_env = "gnu")]
    {
        "gnu"
    }
    #[cfg(target_env = "msvc")]
    {
        "msvc"
    }
    #[cfg(not(any(target_env = "musl", target_env = "gnu", target_env = "msvc")))]
    {
        "none"
    }
}

fn uv_path(root: &Path) -> PathBuf {
    let platform = format!(
        "{}-{}-{}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        target_environment()
    );
    root.join("uv")
        .join(platform)
        .join(if cfg!(windows) { "uv.exe" } else { "uv" })
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

fn prepare_runtime(home: &Path, token: &CancellationToken, progress: &dyn Fn(&str)) -> Result<()> {
    use flate2::read::GzDecoder;

    let root = home.parent().context("CosyVoice 运行目录无效")?;
    let cosyvoice_source = (
        "CosyVoice",
        "https://codeload.github.com/Lourdle/CosyVoice/tar.gz/24c9212eb54e832af4d9b03bcd9157d88acbf6db",
        "f2dbab21a5b539e56cc7de226ef1ed344d649fe3dff769d247970ac21e4394fb",
        home.to_owned(),
    );
    let matcha_source = (
        "Matcha-TTS",
        "https://codeload.github.com/shivammehta25/Matcha-TTS/tar.gz/dd9105b34bf2be2230f4aa1e4769fb586a3c824e",
        "59c43afae79110e4057eaa43f45b5558b556435da8fc85eab83f0c819ba3358d",
        home.join("third_party/Matcha-TTS"),
    );
    fs::create_dir_all(root)?;
    let staging = tempfile::tempdir_in(root)?;
    for (name, url, expected, destination_root) in [cosyvoice_source, matcha_source] {
        check_cancelled(token)?;
        let archive = staging.path().join(format!("{name}.tar.gz"));
        progress(&format!("正在下载 {name} 运行代码…"));
        download_runtime_source(url, &archive, expected, token)?;
        progress(&format!("正在解压 {name} 运行代码…"));
        let extract = staging.path().join(name);
        fs::create_dir_all(&extract)?;
        let mut tar = tar::Archive::new(GzDecoder::new(fs::File::open(&archive)?));
        for item in tar.entries()? {
            check_cancelled(token)?;
            let mut item = item?;
            let entry_type = item.header().entry_type();
            if entry_type.is_symlink()
                || entry_type.is_hard_link()
                || entry_type.is_pax_global_extensions()
            {
                continue;
            }
            ensure!(
                item.header().entry_type().is_file() || item.header().entry_type().is_dir(),
                "运行代码归档包含特殊文件"
            );
            let path = item.path()?.into_owned();
            ensure!(
                path.components().all(|component| matches!(
                    component,
                    std::path::Component::Normal(_) | std::path::Component::CurDir
                )),
                "运行代码归档路径无效"
            );
            let relative = path.components().skip(1).collect::<PathBuf>();
            if relative.as_os_str().is_empty() {
                continue;
            }
            let destination = extract.join(relative);
            ensure!(destination.starts_with(&extract), "运行代码归档路径越界");
            fs::create_dir_all(destination.parent().context("运行代码归档路径无效")?)?;
            if item.header().entry_type().is_dir() {
                fs::create_dir_all(destination)?;
            } else {
                item.unpack(destination)?;
            }
        }
        ensure!(fs::read_dir(&extract)?.next().is_some(), "运行代码归档为空");
        fs::create_dir_all(destination_root.parent().context("运行代码归档路径无效")?)?;
        if name == "CosyVoice" {
            fs::rename(&extract, destination_root)?;
        } else {
            fs::create_dir_all(&destination_root)?;
            fs::rename(extract.join("matcha"), destination_root.join("matcha"))?;
        }
    }
    Ok(())
}

fn download_runtime_source(
    url: &str,
    path: &Path,
    expected: &str,
    token: &CancellationToken,
) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(300))
        .build()?;
    let response = client
        .get(url)
        .send()
        .with_context(|| format!("无法下载 {url}"))?
        .error_for_status()?;
    let mut output = fs::File::create(path)?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = [0; 64 * 1024];
    let mut response = response;
    loop {
        check_cancelled(token)?;
        let count = response.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
    }
    ensure!(
        format!("{:x}", hasher.finalize()) == expected,
        "SHA-256 校验失败：{url}"
    );
    Ok(())
}

fn prepare_environment(
    app_cache: &Path,
    model: &Path,
    token: &CancellationToken,
    progress: &dyn Fn(&str),
) -> Result<()> {
    check_cancelled(token)?;
    progress("正在准备 CosyVoice3 Python 运行环境…");
    let root = app_cache.join("cosyvoice");
    let python = runtime_python(app_cache);
    let python_home = managed_python(&root);
    let marker = root.join("venv/.tfy-bgm-cosyvoice-env");
    let expected = format!("{COSYVOICE_ENV}:cpu\n");
    if python.is_file() && fs::read_to_string(&marker).is_ok_and(|current| current == expected) {
        return Ok(());
    }
    let uv = prepare_python(app_cache, token)?;
    prepare_venv(
        &uv,
        app_cache,
        &python_home,
        &root.join("venv"),
        token,
        progress,
    )?;
    install_environment(&uv, &root, &python, token, progress)?;
    check_environment(&python, app_cache, model, token, progress)?;
    check_cancelled(token)?;
    fs::write(&marker, expected).context("无法标记 CosyVoice CPU 环境已就绪")?;
    Ok(())
}

fn prepare_python(app_cache: &Path, token: &CancellationToken) -> Result<PathBuf> {
    let root = app_cache.join("cosyvoice");
    let python_home = managed_python(&root);
    fs::create_dir_all(&root)?;
    let uv = uv_path(&root);
    if !uv.is_file() {
        install_uv(&uv, &root, token)?;
    }
    if !python_home.is_file() {
        // 全平台都用 uv 安装隔离的 Python，避免依赖系统里的 python3.11。
        let install_dir = root.join("python");
        fs::create_dir_all(&install_dir)?;
        let install_dir = install_dir.to_string_lossy();
        let mut args = vec![
            "python",
            "install",
            "3.11.14",
            "--install-dir",
            install_dir.as_ref(),
        ];
        if cfg!(windows) {
            args.push("--no-registry");
        }
        run_setup(&uv, app_cache, token, &args).context("下载隔离的 Python 3.11 运行环境失败")?;
        ensure!(
            python_home.is_file(),
            "Python 3.11 安装完成但未找到解释器：{}",
            python_home.display()
        );
    }
    Ok(uv)
}

fn prepare_venv(
    uv: &Path,
    app_cache: &Path,
    python_home: &Path,
    venv: &Path,
    token: &CancellationToken,
    progress: &dyn Fn(&str),
) -> Result<()> {
    if !venv.join("pyvenv.cfg").is_file() {
        progress("正在创建 CosyVoice3 Python 环境…");
        let mut command = Command::new(uv);
        command
            .args(["venv", "--python"])
            .arg(python_home)
            .arg(venv)
            .env("UV_CACHE_DIR", app_cache.join("cosyvoice/uv-cache"));
        run_cancellable(&mut command, token, SETUP_COMMAND_TIMEOUT)
            .context("创建 CosyVoice Python 环境失败")?;
    }
    Ok(())
}

const CUDA_PACKAGES: &[&str] = &[
    "onnxruntime-gpu==1.23.2",
    "nvidia-cuda-runtime-cu12==12.6.77",
    "nvidia-cuda-nvrtc-cu12==12.6.77",
    "nvidia-cublas-cu12==12.6.4.1",
    "nvidia-cufft-cu12==11.3.0.4",
    "nvidia-nvjitlink-cu12==12.6.85",
    "nvidia-curand-cu12==10.3.7.77",
    "nvidia-cudnn-cu12==9.8.0.87",
];

pub(super) fn install_managed_cuda(
    model: &Path,
    app_cache: &Path,
    token: &CancellationToken,
    progress: &dyn Fn(&str),
) -> Result<()> {
    let ready = managed_cuda_status(app_cache).context("一键安装仅支持 Windows x64")?;
    if ready {
        return Ok(());
    }
    check_cancelled(token)?;
    ensure!(
        !force_cpu(),
        "当前已强制使用 CPU，请取消 TFY_SPEECH_DEVICE 设置后重启应用"
    );
    progress("正在检查 NVIDIA 驱动和空闲显存…");
    let minimum = minimum_vram_mib("TFY_TTS_MIN_VRAM_MIB", MIN_GPU_MIB);
    let gpu = eligible_nvidia_device(0, token)
        .context("无法检测 NVIDIA 显卡，请确认 NVIDIA 驱动已安装")?
        .context("未检测到可用的 NVIDIA 显卡")?;
    ensure!(
        gpu.free_mib >= minimum,
        "空闲显存不足（{} MiB，至少需要 {minimum} MiB），请关闭占用显存的程序后重试",
        gpu.free_mib
    );
    let home = runtime_source(app_cache, token, progress)?;
    let _setup = RUNTIME_SETUP.lock();
    progress("正在准备 CosyVoice3 CUDA Python 环境…");
    let uv = prepare_python(app_cache, token)?;
    let root = app_cache.join("cosyvoice");
    prepare_venv(
        &uv,
        app_cache,
        &managed_python(&root),
        &managed_cuda_dir(app_cache),
        token,
        progress,
    )?;
    let python = managed_cuda_python(app_cache);
    progress("正在下载并安装 GPU 推理依赖（可能需要几分钟）…");
    let mut command = Command::new(uv);
    command
        .args(["pip", "install", "--python"])
        .arg(&python)
        .args(["torch==2.3.1", "torchaudio==2.3.1"])
        .args(COSYVOICE_COMMON_PACKAGES)
        .args(CUDA_PACKAGES)
        .env("UV_CACHE_DIR", managed_cuda_cache(app_cache));
    let diagnostics = tempfile::NamedTempFile::new().context("无法创建 GPU 安装日志")?;
    command.stderr(diagnostics.reopen()?);
    let mut output = BufReader::new(diagnostics.reopen()?);
    let mut downloads = CudaDownloads::default();
    let mut pending = Vec::new();
    let result = run_cancellable_with_poll(&mut command, token, SETUP_COMMAND_TIMEOUT, || {
        while let Ok(count) = output.read_until(b'\n', &mut pending) {
            if count == 0 || !pending.ends_with(b"\n") {
                break;
            }
            if let Some(stage) = downloads.update(&String::from_utf8_lossy(&pending)) {
                progress(&stage);
            }
            pending.clear();
        }
    });
    if let Err(error) = result {
        let mut diagnostics = diagnostics.reopen()?;
        check_cancelled(token)?;
        use std::io::{Seek, SeekFrom};
        let start = diagnostics
            .metadata()?
            .len()
            .saturating_sub(STDERR_LIMIT as u64);
        diagnostics.seek(SeekFrom::Start(start))?;
        let mut tail = Vec::new();
        diagnostics
            .take(STDERR_LIMIT as u64)
            .read_to_end(&mut tail)?;
        let detail = String::from_utf8_lossy(&tail);
        return Err(error).context(format!("安装 GPU 推理依赖失败：{}", detail.trim()));
    }
    progress("正在用 CosyVoice3 模型验证 CUDA…");
    let model = std::path::absolute(model)?;
    drop(
        Engine::load_worker(&python, &home, &model, app_cache, Some(&gpu.uuid), token)
            .context("CosyVoice3 CUDA 验证失败")?,
    );
    check_cancelled(token)?;
    fs::write(managed_cuda_marker(app_cache), "ready\n").context("无法标记 GPU 环境已就绪")
}

#[derive(Default)]
struct CudaDownloads {
    active: std::collections::BTreeMap<String, String>,
    completed: usize,
}

impl CudaDownloads {
    fn update(&mut self, line: &str) -> Option<String> {
        let line = line.trim();
        if let Some(item) = line.strip_prefix("Downloading ") {
            let name = item.split_whitespace().next()?;
            self.active.insert(name.to_owned(), item.to_owned());
        } else if let Some(item) = line.strip_prefix("Downloaded ") {
            if self
                .active
                .remove(item.split_whitespace().next()?)
                .is_none()
            {
                return None;
            }
            self.completed += 1;
        } else if line.starts_with("Prepared ") {
            return Some("依赖下载完成，正在安装到 GPU 环境…".to_owned());
        } else {
            return None;
        }
        let names = self.active.values().cloned().collect::<Vec<_>>().join("、");
        Some(if names.is_empty() {
            format!("已下载 {} 个包，正在准备安装…", self.completed)
        } else {
            format!(
                "已下载 {} 个包 · 正在下载 {} 个：{names}",
                self.completed,
                self.active.len()
            )
        })
    }
}

const COSYVOICE_COMMON_PACKAGES: &[&str] = &[
    "setuptools==80.9.0",
    "numpy==1.26.4",
    "transformers==4.51.3",
    "modelscope==1.20.0",
    "HyperPyYAML==1.2.3",
    "openai-whisper==20250625",
    "soundfile==0.12.1",
    "librosa==0.10.2",
    "conformer==0.3.2",
    "diffusers==0.29.0",
    "x-transformers==2.11.24",
    "hydra-core==1.3.2",
    "lightning==2.2.4",
    "rich==13.7.1",
    "gdown==5.1.0",
    "matplotlib==3.7.5",
    "wget==3.2",
    "pyarrow==18.1.0",
    "pyworld==0.3.5",
    "inflect",
    "pydantic",
];

/// 清理旧环境的推理包（含 DirectML），避免同一个 venv 存在多份 ONNX Runtime。
fn install_environment(
    uv: &Path,
    root: &Path,
    python: &Path,
    token: &CancellationToken,
    progress: &dyn Fn(&str),
) -> Result<()> {
    let mut uninstall = Command::new(uv);
    uninstall
        .args(["pip", "uninstall", "--python"])
        .arg(python)
        .args([
            "torch",
            "torchaudio",
            "onnxruntime",
            "onnxruntime-gpu",
            "onnxruntime-directml",
        ]);
    run_cancellable(&mut uninstall, token, SETUP_COMMAND_TIMEOUT)
        .context("无法清理旧的 CosyVoice 推理依赖")?;

    progress("正在安装 CPU 推理依赖（可选 CUDA 环境由用户单独配置）…");
    // 不写死索引地址：uv 默认使用 PyPI，并尊重 UV_INDEX_URL 等用户/镜像配置。
    let mut command = Command::new(uv);
    command
        .args(["pip", "install", "--python"])
        .arg(python)
        .args(["torch==2.3.1", "torchaudio==2.3.1", "onnxruntime==1.23.2"])
        .args(COSYVOICE_COMMON_PACKAGES)
        .env("UV_CACHE_DIR", root.join("uv-cache"));
    run_cancellable(&mut command, token, SETUP_COMMAND_TIMEOUT)
        .context("安装 CosyVoice 推理依赖失败，请检查网络后重试")
}

/// uv wheel 内的可执行文件路径；版本需与 `uv_artifact` 的下载地址保持一致。
fn uv_script_path(executable: &str) -> String {
    format!("uv-0.9.26.data/scripts/{executable}")
}

fn install_uv(uv: &Path, root: &Path, token: &CancellationToken) -> Result<()> {
    check_cancelled(token)?;
    fs::create_dir_all(uv.parent().context("uv 安装目录无效")?)?;
    if !cfg!(windows)
        && let Some(python) = env::var_os("PATH").and_then(|path| {
            env::split_paths(&path)
                .map(|directory| directory.join("uv"))
                .find(|candidate| candidate.is_file())
        })
    {
        fs::copy(python, uv)?;
        #[cfg(unix)]
        make_executable(uv)?;
        check_cancelled(token)?;
        return Ok(());
    }
    let artifact = uv_artifact(
        std::env::consts::OS,
        std::env::consts::ARCH,
        target_environment(),
    )?;
    let wheel = root.join("uv.whl");
    download_runtime_source(artifact.url, &wheel, artifact.sha256, token)?;
    let mut archive = zip::ZipArchive::new(fs::File::open(&wheel)?)?;
    // wheel 内条目固定使用 `/`；不能用 Path::join，Windows 会拼出反斜杠导致查不到。
    let mut source = archive.by_name(&uv_script_path(artifact.executable))?;
    let mut output = fs::File::create(uv)?;
    io::copy(&mut source, &mut output)?;
    ensure!(output.metadata()?.len() > 0, "uv 运行环境安装文件为空");
    drop(output);
    #[cfg(unix)]
    make_executable(uv)?;
    check_cancelled(token)?;
    Ok(())
}

fn check_environment(
    python: &Path,
    app_cache: &Path,
    model: &Path,
    token: &CancellationToken,
    progress: &dyn Fn(&str),
) -> Result<()> {
    progress("正在验证 CosyVoice3 环境及模型…");
    let home = runtime_home(app_cache);
    let mut command = worker_command(python, &home, model, app_cache, None);
    command.arg("--self-test");
    run_cancellable(&mut command, token, ENV_CHECK_TIMEOUT).context("CosyVoice 环境及模型验证失败")
}

fn worker_command(
    python: &Path,
    home: &Path,
    model: &Path,
    app_cache: &Path,
    gpu_uuid: Option<&str>,
) -> Command {
    let mut command = bgm_core::process::command(python);
    command
        .args(["-u", "-c", PYTHON_WORKER])
        .arg(home)
        .arg(model)
        .env("HF_HUB_OFFLINE", "1")
        .env("MODELSCOPE_OFFLINE", "1")
        .env("PYTHONUTF8", "1")
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONPATH", home.join("third_party/Matcha-TTS"))
        .env("MODELSCOPE_CACHE", app_cache.join("cosyvoice/modelscope"))
        .env(
            "TFY_COSYVOICE_USE_CUDA",
            if gpu_uuid.is_some() { "1" } else { "0" },
        )
        .env("CUDA_VISIBLE_DEVICES", gpu_uuid.unwrap_or(""))
        .env(
            "TFY_COSYVOICE_MANAGED_CUDA",
            if gpu_uuid.is_some() && python == managed_cuda_python(app_cache) {
                "1"
            } else {
                "0"
            },
        );
    command
}

fn cuda_python(path: Option<std::ffi::OsString>) -> Option<PathBuf> {
    path.map(PathBuf::from)
        .filter(|path| path.is_absolute() && path.is_file())
}

fn run_setup(uv: &Path, cache: &Path, token: &CancellationToken, args: &[&str]) -> Result<()> {
    let mut command = Command::new(uv);
    command
        .args(args)
        .env("UV_CACHE_DIR", cache.join("cosyvoice/uv-cache"));
    run_cancellable(&mut command, token, SETUP_COMMAND_TIMEOUT)
}

fn run_cancellable(
    command: &mut Command,
    token: &CancellationToken,
    timeout: Duration,
) -> Result<()> {
    run_cancellable_with_poll(command, token, timeout, || {})
}

fn run_cancellable_with_poll(
    command: &mut Command,
    token: &CancellationToken,
    timeout: Duration,
    mut poll: impl FnMut(),
) -> Result<()> {
    check_cancelled(token)?;
    bgm_core::process::suppress_console(command);
    let mut child = command.spawn()?;
    let deadline = Instant::now() + timeout;
    loop {
        if token.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow::anyhow!("CosyVoice 环境准备已取消"));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let _ = child.kill();
            let _ = child.wait();
            bail!("CosyVoice 环境准备命令超时（{} 秒）", timeout.as_secs());
        }
        let status = child.try_wait()?;
        poll();
        if let Some(status) = status {
            ensure!(status.success(), "CosyVoice 环境准备命令失败（{status}）");
            return Ok(());
        }
        thread::sleep(remaining.min(Duration::from_millis(100)));
    }
}

#[derive(Serialize)]
struct GenerateRequest<'a> {
    text: &'a str,
    prompt_text: &'a str,
    prompt_path: &'a Path,
    output_path: &'a Path,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Reply {
    Ready { sample_rate: u32 },
    Generated,
    Error { message: String },
}

struct CpuFallback {
    home: PathBuf,
    model: PathBuf,
    app_cache: PathBuf,
}

impl CpuFallback {
    fn load(&self, token: &CancellationToken, progress: &dyn Fn(&str)) -> Result<Engine> {
        {
            let _setup = RUNTIME_SETUP.lock();
            prepare_environment(&self.app_cache, &self.model, token, progress)?;
        }
        progress("正在启动 CosyVoice3 CPU 推理进程…");
        Engine::load_worker(
            &runtime_python(&self.app_cache),
            &self.home,
            &self.model,
            &self.app_cache,
            None,
            token,
        )
    }
}

pub(super) struct Engine {
    child: Option<Child>,
    requests: Option<SyncSender<Vec<u8>>>,
    replies: Receiver<Result<Reply>>,
    stderr_tail: Arc<Mutex<Vec<u8>>>,
    // Only a successfully initialized CUDA worker can switch to CPU.
    cpu_fallback: Option<CpuFallback>,
    managed_cuda: bool,
}

impl Engine {
    pub(super) fn load(
        path: &Path,
        app_cache: &Path,
        token: &CancellationToken,
        progress: &dyn Fn(&str),
    ) -> Result<Self> {
        check_cancelled(token)?;
        let model = std::path::absolute(path).context("CosyVoice3 模型路径无效")?;
        for name in [
            "cosyvoice3.yaml",
            "CosyVoice-BlankEN/config.json",
            "CosyVoice-BlankEN/tokenizer_config.json",
            "CosyVoice-BlankEN/vocab.json",
            "CosyVoice-BlankEN/merges.txt",
            "frontend/campplus.onnx",
            "frontend/speech_tokenizer_v3.onnx",
            "llm/model.onnx",
            "llm/model.onnx.data",
            "flow/flow_conditioner.onnx",
            "flow/flow_dit_step.onnx",
            "flow/flow_dit_step.onnx.data",
            "hift/model.onnx",
        ] {
            check_cancelled(token)?;
            let file = model.join(name);
            ensure!(
                fs::metadata(&file).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0),
                "CosyVoice3 模型文件缺失或为空：{}",
                file.display()
            );
        }
        let home = runtime_source(app_cache, token, progress)?;
        let cpu = CpuFallback {
            home,
            model,
            app_cache: app_cache.to_owned(),
        };
        check_cancelled(token)?;
        if !force_cpu()
            && let Some(python) = selected_cuda_python(app_cache)
        {
            let minimum = minimum_vram_mib("TFY_TTS_MIN_VRAM_MIB", MIN_GPU_MIB);
            match eligible_nvidia_device(minimum, token) {
                Ok(Some(gpu)) => {
                    progress("正在启动 CosyVoice3 CUDA 推理进程…");
                    let result = Self::load_worker(
                        &python,
                        &cpu.home,
                        &cpu.model,
                        &cpu.app_cache,
                        Some(&gpu.uuid),
                        token,
                    );
                    return match result {
                        Ok(mut engine) => {
                            engine.cpu_fallback = Some(cpu);
                            Ok(engine)
                        }
                        Err(error) => {
                            check_cancelled(token)?;
                            progress("CosyVoice CUDA 初始化失败，正在回退 CPU…");
                            eprintln!("CosyVoice CUDA 初始化失败：{error:#}");
                            cpu.load(token, progress)
                        }
                    };
                }
                Ok(None) => {}
                Err(error) => {
                    check_cancelled(token)?;
                    progress("GPU 显存检测失败，使用 CPU 推理…");
                    eprintln!("CosyVoice GPU 探测失败：{error:#}");
                }
            }
        }
        cpu.load(token, progress)
    }

    fn load_worker(
        python: &Path,
        home: &Path,
        model: &Path,
        app_cache: &Path,
        gpu_uuid: Option<&str>,
        token: &CancellationToken,
    ) -> Result<Self> {
        check_cancelled(token)?;
        let mut engine = Self::spawn(python, home, model, app_cache, gpu_uuid)?;
        let result = engine
            .wait_for_reply(LOAD_TIMEOUT, token)
            .and_then(|reply| match reply {
                Reply::Ready {
                    sample_rate: SAMPLE_RATE,
                } => Ok(()),
                Reply::Ready { sample_rate } => {
                    bail!("CosyVoice3 模型采样率应为 24000 Hz，实际为 {sample_rate} Hz")
                }
                _ => bail!("CosyVoice3 Python 进程返回了无效的启动响应"),
            });
        engine.finish(
            result,
            "CosyVoice3 加载失败，请检查 Lourdle CosyVoice 运行环境及依赖",
            token,
        )?;
        Ok(engine)
    }

    pub(super) fn device(&self) -> &'static str {
        if self.cpu_fallback.is_some() {
            "cuda"
        } else {
            "cpu"
        }
    }

    pub(super) fn uses_managed_cuda(&self) -> bool {
        self.managed_cuda
    }

    pub(super) fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    pub(super) fn is_alive(&mut self) -> bool {
        matches!(self.child.as_mut().map(Child::try_wait), Some(Ok(None)))
    }

    pub(super) fn generate(
        &mut self,
        text: &str,
        prompt_text: &str,
        prompt_path: &Path,
        output_path: &Path,
        token: &CancellationToken,
        on_fallback: &dyn Fn(),
    ) -> Result<Vec<f32>> {
        if token.is_cancelled() {
            self.stop();
            check_cancelled(token)?;
        }
        let prompt_path = std::path::absolute(prompt_path).context("参考音频路径无效")?;
        let output_path = std::path::absolute(output_path).context("语音输出路径无效")?;
        let mut line = serde_json::to_vec(&GenerateRequest {
            text,
            prompt_text,
            prompt_path: &prompt_path,
            output_path: &output_path,
        })
        .context("无法编码 CosyVoice3 请求")?;
        line.push(b'\n');
        if self.cpu_fallback.is_none() {
            return self.generate_once(line, &output_path, token);
        }
        match self.generate_once(line.clone(), &output_path, token) {
            Ok(samples) => Ok(samples),
            Err(error) => {
                // generate_once has already stopped and reaped the failed worker.
                check_cancelled(token)?;
                let Some(cpu) = self.cpu_fallback.take() else {
                    return Err(error);
                };
                eprintln!("CosyVoice CUDA 推理失败，正在回退 CPU 并重试当前片段：{error:#}");
                match fs::remove_file(&output_path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error).context("无法清理 CosyVoice3 重试前的输出"),
                }
                let engine = cpu.load(token, &|stage| eprintln!("[cosyvoice] {stage}"));
                check_cancelled(token)?;
                *self = engine.context("CosyVoice3 CUDA 推理失败后无法启动 CPU 进程")?;
                on_fallback();
                self.generate_once(line, &output_path, token)
            }
        }
    }

    fn generate_once(
        &mut self,
        line: Vec<u8>,
        output_path: &Path,
        token: &CancellationToken,
    ) -> Result<Vec<f32>> {
        let result = (|| {
            check_cancelled(token)?;
            self.requests
                .as_ref()
                .context("CosyVoice3 Python 进程已关闭")?
                .try_send(line)
                .context("无法向 CosyVoice3 Python 进程提交请求")?;
            ensure!(
                matches!(
                    self.wait_for_reply(GENERATE_TIMEOUT, token)?,
                    Reply::Generated
                ),
                "CosyVoice3 Python 进程返回了无效的推理响应"
            );
            let wave = speech_audio::read_wave(output_path)
                .context("CosyVoice3 未生成有效的单声道 PCM WAV")?;
            ensure!(
                wave.sample_rate() == self.sample_rate() as i32,
                "CosyVoice3 输出采样率须为 24000 Hz"
            );
            let samples = wave.samples();
            ensure!(
                !samples.is_empty() && samples.iter().all(|sample| sample.is_finite()),
                "CosyVoice3 未生成有效音频采样"
            );
            Ok(samples.to_vec())
        })();
        self.finish(result, "CosyVoice3 推理失败", token)
    }

    fn spawn(
        python: &Path,
        home: &Path,
        model: &Path,
        app_cache: &Path,
        gpu_uuid: Option<&str>,
    ) -> Result<Self> {
        let mut command = worker_command(python, home, model, app_cache, gpu_uuid);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command.spawn().with_context(|| {
            format!(
                "无法启动 CosyVoice3 Python 推理进程（{}），请清理应用缓存后重试",
                python.display()
            )
        })?;

        let (requests, pending) = mpsc::sync_channel::<Vec<u8>>(1);
        let (events, replies) = mpsc::channel();
        let mut engine = Self {
            child: Some(child),
            requests: Some(requests),
            replies,
            stderr_tail: Arc::default(),
            cpu_fallback: None,
            managed_cuda: gpu_uuid.is_some() && python == managed_cuda_python(app_cache),
        };
        // Each thread owns its pipe. In particular, writing a large request to
        // a stalled worker must never block the caller's cancellation polling.
        let child = engine.child.as_mut().unwrap();
        let mut stdin = child
            .stdin
            .take()
            .context("CosyVoice3 Python stdin 不可用")?;
        let stdout = child
            .stdout
            .take()
            .context("CosyVoice3 Python stdout 不可用")?;
        let stderr = child
            .stderr
            .take()
            .context("CosyVoice3 Python stderr 不可用")?;
        let tail = Arc::clone(&engine.stderr_tail);
        thread::Builder::new()
            .name("cosyvoice-stderr".into())
            .spawn(move || drain_stderr(stderr, &tail))?;
        let write_events = events.clone();
        thread::Builder::new()
            .name("cosyvoice-stdin".into())
            .spawn(move || {
                while let Ok(line) = pending.recv() {
                    if let Err(error) = stdin.write_all(&line) {
                        let _ = write_events
                            .send(Err(error).context("写入 CosyVoice3 Python 请求失败"));
                        return;
                    }
                }
            })?;
        thread::Builder::new()
            .name("cosyvoice-stdout".into())
            .spawn(move || {
                let mut stdout = BufReader::new(stdout);
                loop {
                    let reply = read_reply(&mut stdout);
                    let terminal = reply.is_err() || matches!(reply, Ok(Reply::Error { .. }));
                    if events.send(reply).is_err() || terminal {
                        return;
                    }
                }
            })?;
        Ok(engine)
    }

    fn wait_for_reply(&mut self, timeout: Duration, token: &CancellationToken) -> Result<Reply> {
        let deadline = Instant::now() + timeout;
        loop {
            check_cancelled(token)?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            ensure!(
                !remaining.is_zero(),
                "等待 CosyVoice3 Python 响应超时（{} 秒）",
                timeout.as_secs()
            );
            match self.replies.recv_timeout(remaining.min(POLL_INTERVAL)) {
                Ok(reply) => {
                    check_cancelled(token)?;
                    return match reply? {
                        Reply::Error { message } => bail!("{message}"),
                        reply => Ok(reply),
                    };
                }
                Err(RecvTimeoutError::Disconnected) => bail!("CosyVoice3 Python 通信已中断"),
                Err(RecvTimeoutError::Timeout) => {
                    let child = self
                        .child
                        .as_mut()
                        .context("CosyVoice3 Python 进程已关闭")?;
                    if let Some(status) = child
                        .try_wait()
                        .context("无法查询 CosyVoice3 Python 进程状态")?
                    {
                        bail!("CosyVoice3 Python 进程已退出（{status}）");
                    }
                }
            }
        }
    }

    fn finish<T>(
        &mut self,
        result: Result<T>,
        stage: &str,
        token: &CancellationToken,
    ) -> Result<T> {
        if token.is_cancelled() {
            self.stop();
            check_cancelled(token)?;
        }
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                self.stop();
                let tail = self
                    .stderr_tail
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                let tail = String::from_utf8_lossy(&tail);
                if tail.trim().is_empty() {
                    Err(error).context(stage.to_owned())
                } else {
                    Err(error).with_context(|| {
                        format!("{stage}\nPython stderr（末尾）：\n{}", tail.trim())
                    })
                }
            }
        }
    }

    fn stop(&mut self) {
        self.requests.take();
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}

fn read_reply(stdout: &mut impl BufRead) -> Result<Reply> {
    let mut line = Vec::new();
    loop {
        line.clear();
        stdout
            .take(FRAME_LIMIT + 1)
            .read_until(b'\n', &mut line)
            .context("读取 CosyVoice3 Python 响应失败")?;
        ensure!(!line.is_empty(), "CosyVoice3 Python 已关闭响应管道");
        ensure!(
            line.len() as u64 <= FRAME_LIMIT && line.ends_with(b"\n"),
            "CosyVoice3 Python 响应帧不完整或过长"
        );
        if let Some(payload) = line.strip_prefix(FRAME_PREFIX) {
            return serde_json::from_slice(payload).context("无法解析 CosyVoice3 Python 响应");
        }
    }
}

fn drain_stderr(mut stderr: impl Read, tail: &Mutex<Vec<u8>>) {
    let mut buffer = [0; 4096];
    loop {
        match stderr.read(&mut buffer) {
            Ok(0) => return,
            Ok(len) => {
                let mut tail = tail.lock().unwrap_or_else(|error| error.into_inner());
                let excess = (tail.len() + len).saturating_sub(STDERR_LIMIT);
                tail.drain(..excess);
                tail.extend_from_slice(&buffer[..len]);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cuda_progress_tracks_parallel_downloads_and_installation() {
        let mut downloads = CudaDownloads::default();
        assert!(downloads.update("Resolved 121 packages in 2.98s").is_none());
        assert!(
            downloads
                .update("Downloading onnxruntime-gpu (233.2MiB)")
                .unwrap()
                .contains("233.2MiB")
        );
        assert!(
            downloads
                .update("Downloading nvidia-cudnn-cu12 (652.9MiB)")
                .unwrap()
                .contains("正在下载 2 个")
        );
        let stage = downloads.update(" Downloaded onnxruntime-gpu").unwrap();
        assert!(stage.contains("已下载 1 个包"));
        assert!(!stage.contains("onnxruntime-gpu"));
        assert!(downloads.update("Downloaded onnxruntime-gpu").is_none());
        assert!(
            downloads
                .update("Prepared 121 packages in 2m")
                .unwrap()
                .contains("正在安装")
        );
    }

    #[test]
    fn managed_cuda_requires_a_verified_environment() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        if !cfg!(all(windows, target_arch = "x86_64")) {
            assert_eq!(managed_cuda_status(root), None);
            return;
        }
        assert_eq!(managed_cuda_status(root), Some(false));
        let python = managed_cuda_python(root);
        fs::create_dir_all(python.parent().unwrap()).unwrap();
        fs::write(&python, b"test").unwrap();
        assert_eq!(managed_cuda_status(root), Some(false));
        fs::write(managed_cuda_marker(root), "").unwrap();
        assert_eq!(managed_cuda_status(root), Some(false));
        fs::write(managed_cuda_marker(root), "ready\n").unwrap();
        assert_eq!(managed_cuda_status(root), Some(true));
        fs::remove_file(python).unwrap();
        assert_eq!(managed_cuda_status(root), Some(false));
    }

    #[test]
    #[cfg(all(windows, target_arch = "x86_64"))]
    fn removes_unverified_cuda_without_touching_cpu() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let cpu = runtime_python(root);
        fs::create_dir_all(cpu.parent().unwrap()).unwrap();
        fs::write(&cpu, b"cpu").unwrap();
        fs::create_dir_all(managed_cuda_dir(root)).unwrap();
        fs::create_dir_all(managed_cuda_cache(root)).unwrap();
        assert_eq!(managed_cuda_status(root), Some(false));
        assert!(managed_cuda_present(root));
        remove_managed_cuda(root).unwrap();
        assert!(!managed_cuda_present(root));
        assert_eq!(fs::read(&cpu).unwrap(), b"cpu");
    }

    #[test]
    fn cuda_python_requires_an_absolute_file_path() {
        let dir = tempfile::tempdir().unwrap();
        let python = dir.path().join("python.exe");
        fs::write(&python, b"test").unwrap();
        assert_eq!(
            cuda_python(Some(python.clone().into_os_string())),
            Some(python)
        );
        assert!(cuda_python(None).is_none());
        assert!(cuda_python(Some("python.exe".into())).is_none());
        assert!(cuda_python(Some(dir.path().as_os_str().to_owned())).is_none());
        assert!(cuda_python(Some(dir.path().join("missing.exe").into_os_string())).is_none());
    }

    #[test]
    fn worker_environment_pins_the_device_even_for_cpu_self_test() {
        let path = Path::new("test");
        for (uuid, expected_flag) in [(None, "0"), (Some("GPU-test"), "1")] {
            let command = worker_command(path, path, path, path, uuid);
            let value = |key| {
                command.get_envs().find_map(|(name, value)| {
                    (name == std::ffi::OsStr::new(key))
                        .then_some(value)
                        .flatten()
                })
            };
            assert_eq!(value("TFY_COSYVOICE_USE_CUDA").unwrap(), expected_flag);
            assert_eq!(value("CUDA_VISIBLE_DEVICES").unwrap(), uuid.unwrap_or(""));
        }
    }

    fn test_cuda_worker(root: &Path) -> (Engine, Receiver<Vec<u8>>, mpsc::Sender<Result<Reply>>) {
        let python = runtime_python(root);
        fs::create_dir_all(python.parent().unwrap()).unwrap();
        fs::write(python, b"invalid interpreter").unwrap();
        fs::write(
            root.join("cosyvoice/venv/.tfy-bgm-cosyvoice-env"),
            format!("{COSYVOICE_ENV}:cpu\n"),
        )
        .unwrap();
        let (requests, pending) = mpsc::sync_channel(1);
        let (events, replies) = mpsc::channel();
        let engine = Engine {
            child: Some(long_running_command().spawn().unwrap()),
            requests: Some(requests),
            replies,
            stderr_tail: Arc::default(),
            managed_cuda: false,
            cpu_fallback: Some(CpuFallback {
                home: root.to_owned(),
                model: root.to_owned(),
                app_cache: root.to_owned(),
            }),
        };
        (engine, pending, events)
    }

    #[test]
    fn cuda_failure_reaps_worker_and_consumes_fallback_before_retry() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("speech.wav");
        fs::write(&output, b"partial WAV").unwrap();
        let (mut engine, pending, events) = test_cuda_worker(dir.path());
        events
            .send(Ok(Reply::Error {
                message: "CUDA failed".into(),
            }))
            .unwrap();
        let error = engine
            .generate(
                "text",
                "prompt",
                &output,
                &output,
                &CancellationToken::new(),
                &|| panic!("failed CPU startup must not publish CPU inference"),
            )
            .unwrap_err();
        assert!(error.to_string().contains("无法启动 CPU 进程"), "{error:#}");
        assert!(!engine.is_alive());
        assert!(engine.cpu_fallback.is_none());
        assert!(!output.exists());
        assert!(pending.try_recv().is_ok());
        assert!(matches!(
            pending.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn cancellation_during_cuda_request_does_not_start_cpu() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("speech.wav");
        fs::write(&output, b"partial WAV").unwrap();
        let (mut engine, pending, events) = test_cuda_worker(dir.path());
        let token = CancellationToken::new();
        let cancel = token.clone();
        let responder = thread::spawn(move || {
            pending.recv_timeout(Duration::from_secs(3)).unwrap();
            cancel.cancel();
            let _ = events.send(Ok(Reply::Error {
                message: "CUDA failed".into(),
            }));
        });
        let error = engine
            .generate("text", "prompt", &output, &output, &token, &|| {
                panic!("cancellation must not publish CPU inference");
            })
            .unwrap_err();
        responder.join().unwrap();
        assert!(error.to_string().contains("已取消"), "{error:#}");
        assert!(!engine.is_alive());
        assert!(engine.cpu_fallback.is_some());
        assert!(output.exists());
    }

    #[test]
    fn uv_wheel_script_path_uses_forward_slashes() {
        assert_eq!(uv_script_path("uv.exe"), "uv-0.9.26.data/scripts/uv.exe");
        assert_eq!(uv_script_path("uv"), "uv-0.9.26.data/scripts/uv");
    }

    #[test]
    fn managed_python_matches_python_build_standalone_layout() {
        let python = managed_python(Path::new("cache"));
        let text = python.to_string_lossy().replace('\\', "/");
        assert!(
            text.contains(&format!(
                "cpython-3.11.14-{}-{}-",
                std::env::consts::OS,
                std::env::consts::ARCH
            )),
            "{text}"
        );
        assert!(text.ends_with(if cfg!(windows) {
            "python.exe"
        } else {
            "bin/python3"
        }));
    }

    #[test]
    fn uv_artifacts_match_operating_system_architecture_and_libc() {
        let cases = [
            ("windows", "x86_64", "msvc", "win_amd64.whl", "uv.exe"),
            ("windows", "aarch64", "msvc", "win_arm64.whl", "uv.exe"),
            ("macos", "x86_64", "none", "macosx_10_12_x86_64.whl", "uv"),
            ("macos", "aarch64", "none", "macosx_11_0_arm64.whl", "uv"),
            (
                "linux",
                "x86_64",
                "gnu",
                "manylinux_2_17_x86_64.manylinux2014_x86_64.whl",
                "uv",
            ),
            ("linux", "x86_64", "musl", "musllinux_1_1_x86_64.whl", "uv"),
            (
                "linux",
                "aarch64",
                "gnu",
                "manylinux2014_aarch64.musllinux_1_1_aarch64.whl",
                "uv",
            ),
            (
                "linux",
                "aarch64",
                "musl",
                "manylinux2014_aarch64.musllinux_1_1_aarch64.whl",
                "uv",
            ),
        ];
        for (os, arch, environment, filename, executable) in cases {
            let artifact = uv_artifact(os, arch, environment).unwrap();
            assert!(artifact.url.ends_with(filename), "{}", artifact.url);
            assert_eq!(artifact.executable, executable);
            assert_eq!(artifact.sha256.len(), 64);
            assert!(artifact.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn unsupported_uv_platform_returns_an_explicit_error() {
        let error = uv_artifact("freebsd", "x86_64", "gnu").unwrap_err();
        assert!(error.to_string().contains("暂不支持自动准备"));
    }

    #[test]
    fn uv_cache_is_partitioned_by_platform() {
        let path = uv_path(Path::new("cache"));
        let text = path.to_string_lossy();
        assert!(text.contains(std::env::consts::OS));
        assert!(text.contains(std::env::consts::ARCH));
        assert!(path.starts_with("cache"));
    }

    #[cfg(unix)]
    #[test]
    fn installed_uv_is_executable() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let uv = root.path().join("uv");
        fs::write(&uv, b"test").unwrap();
        make_executable(&uv).unwrap();
        assert_eq!(
            fs::metadata(&uv).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    fn long_running_command() -> Command {
        #[cfg(windows)]
        {
            // 直接启动 ping，避免经 cmd 中转后 kill 只杀掉 shell、留下孤儿子进程。
            let mut command = Command::new("ping");
            command.args(["-n", "10", "127.0.0.1"]);
            command.stdout(Stdio::null());
            command.stderr(Stdio::null());
            command
        }
        #[cfg(unix)]
        {
            let mut command = Command::new("sleep");
            command.arg("5");
            command
        }
    }

    #[test]
    fn setup_commands_time_out_and_stop_the_child() {
        let started = Instant::now();
        let error = run_cancellable(
            &mut long_running_command(),
            &CancellationToken::new(),
            Duration::from_millis(150),
        )
        .unwrap_err();
        assert!(error.to_string().contains("超时"));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn setup_commands_can_be_cancelled() {
        let token = CancellationToken::new();
        let cancel = token.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            cancel.cancel();
        });
        let started = Instant::now();
        let error = run_cancellable(&mut long_running_command(), &token, Duration::from_secs(10))
            .unwrap_err();
        assert!(error.to_string().contains("已取消"));
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
