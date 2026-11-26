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
use crate::speech_audio;
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

fn runtime_paths(
    model: &Path,
    app_cache: &Path,
    token: &CancellationToken,
    progress: &dyn Fn(&str),
) -> Result<(PathBuf, PathBuf)> {
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
    prepare_environment(app_cache, model, token, progress)?;
    Ok((home, runtime_python(app_cache)))
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
            if item.header().entry_type().is_symlink() || item.header().entry_type().is_hard_link()
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
            fs::rename(extract.join("matcha"), destination_root)?;
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
    let variant = environment_variant(progress);
    let expected = format!("{COSYVOICE_ENV}:{variant}\n");
    if python.is_file() && fs::read_to_string(&marker).is_ok_and(|current| current == expected) {
        return Ok(());
    }
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
    if !root.join("venv/pyvenv.cfg").is_file() {
        progress("正在创建 CosyVoice3 Python 环境…");
        let python_home = python_home.to_string_lossy();
        let venv = root.join("venv");
        run_setup(
            &uv,
            app_cache,
            token,
            &[
                "venv",
                "--python",
                python_home.as_ref(),
                venv.to_string_lossy().as_ref(),
            ],
        )
        .context("创建 CosyVoice Python 环境失败")?;
    }
    let mut last_error = None;
    for candidate in std::iter::once(variant).chain(fallback_variants(variant).iter().copied()) {
        progress(&format!("正在准备 {candidate} 推理环境…"));
        let installed = install_environment(&uv, &root, &python, candidate, token, progress)
            .and_then(|()| check_environment(&python, app_cache, model, token, progress));
        match installed {
            Ok(()) => {
                fs::write(&marker, format!("{COSYVOICE_ENV}:{candidate}\n"))?;
                return Ok(());
            }
            Err(error) => {
                eprintln!("CosyVoice {candidate} 环境准备失败：{error:#}");
                last_error = Some(error);
            }
        }
    }
    Err(last_error.context("CosyVoice 推理环境准备失败")?)
}

fn gpu_memory_mib() -> Option<u64> {
    let output = bgm_core::process::command(Path::new("nvidia-smi"))
        .args(["--query-gpu=memory.free", "--format=csv,noheader,nounits"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse::<u64>().ok())
        .max()
}

fn environment_variant(progress: &dyn Fn(&str)) -> &'static str {
    match gpu_memory_mib() {
        Some(mib) if mib >= MIN_GPU_MIB => {
            progress(&format!(
                "检测到 NVIDIA 显卡（{mib} MiB），将用 DirectML 加速 Flow/HiFT…"
            ));
            "dml"
        }
        Some(mib) => {
            progress(&format!(
                "显卡可用显存 {mib} MiB 低于 {MIN_GPU_MIB} MiB，将使用 CPU 推理…"
            ));
            "cpu"
        }
        None => {
            progress("未检测到可用 NVIDIA 显卡，将使用 CPU 推理…");
            "cpu"
        }
    }
}

fn fallback_variants(variant: &str) -> &'static [&'static str] {
    match variant {
        "dml" => &["cpu"],
        _ => &[],
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

/// 按设备变体安装推理包；切换变体前先卸载旧的 torch/onnxruntime，避免两份实现共存。
fn install_environment(
    uv: &Path,
    root: &Path,
    python: &Path,
    variant: &str,
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
    let _ = run_cancellable(&mut uninstall, token, SETUP_COMMAND_TIMEOUT);

    progress(match variant {
        "dml" => "正在安装 DirectML 版 ONNX Runtime（Flow/HiFT 将使用显卡）…",
        _ => "正在安装 CPU 推理依赖…",
    });
    // 不写死索引地址：uv 默认使用 PyPI，并尊重 UV_INDEX_URL 等用户/镜像配置。
    let mut command = Command::new(uv);
    command.args(["pip", "install", "--python"]).arg(python);
    match variant {
        "dml" => {
            // PyTorch 用 CPU 版（DirectML 不依赖 CUDA/cuDNN）；LLM/Flow/HiFT 的
            // ONNX 会话由 worker 按可用显存尝试 DirectML，失败时自动回退 CPU。
            command.args([
                "torch==2.3.1",
                "torchaudio==2.3.1",
                "onnxruntime-directml==1.24.4",
            ]);
        }
        _ => {
            command.args(["torch==2.3.1", "torchaudio==2.3.1", "onnxruntime==1.23.2"]);
        }
    }
    command
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
    let args = ["-u", "-c", PYTHON_WORKER];
    let mut command = Command::new(python);
    command
        .args(args)
        .arg(&home)
        .arg(model)
        .arg("--self-test")
        .env("PYTHONUTF8", "1")
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONPATH", home.join("third_party/Matcha-TTS"))
        .env("MODELSCOPE_CACHE", app_cache.join("cosyvoice/modelscope"))
        .env("HF_HUB_OFFLINE", "1")
        .env("MODELSCOPE_OFFLINE", "1");
    run_cancellable(&mut command, token, ENV_CHECK_TIMEOUT).context("CosyVoice 环境及模型验证失败")
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
        if let Some(status) = child.try_wait()? {
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

pub(super) struct Engine {
    child: Option<Child>,
    requests: Option<SyncSender<Vec<u8>>>,
    replies: Receiver<Result<Reply>>,
    stderr_tail: Arc<Mutex<Vec<u8>>>,
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
        let (home, python) = runtime_paths(&model, app_cache, token, progress)?;
        check_cancelled(token)?;
        progress("正在启动 CosyVoice3 推理进程…");
        let mut engine = Self::spawn(&python, &home, &model, app_cache)?;
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
    ) -> Result<Vec<f32>> {
        let result = (|| {
            check_cancelled(token)?;
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
            let wave = speech_audio::read_wave(&output_path)
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

    fn spawn(python: &Path, home: &Path, model: &Path, app_cache: &Path) -> Result<Self> {
        let child = bgm_core::process::command(python)
            .args(["-u", "-c", PYTHON_WORKER])
            .arg(home)
            .arg(model)
            .env("HF_HUB_OFFLINE", "1")
            .env("MODELSCOPE_OFFLINE", "1")
            .env("PYTHONUTF8", "1")
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONPATH", home.join("third_party/Matcha-TTS"))
            .env("MODELSCOPE_CACHE", app_cache.join("cosyvoice/modelscope"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| {
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
