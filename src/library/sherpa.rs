//! sherpa-onnx 2.0.2 动态库发现、内置自动安装与版本校验。

use std::ffi::{CStr, c_char};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use libloading::Library;
use sha2::{Digest, Sha256};
use tempfile::Builder;

pub const RUNTIME_VERSION: &str = "2.0.2";
const DOWNLOAD_CONNECT_TIMEOUT_SECS: &str = "15";
const DOWNLOAD_STALL_TIMEOUT: Duration = Duration::from_secs(45);
const DOWNLOAD_TOTAL_TIMEOUT: Duration = Duration::from_secs(10 * 60);

const ENGINE_FILE: &str = if cfg!(target_os = "macos") {
    "libsherpa-onnx-c-api.dylib"
} else if cfg!(target_os = "windows") {
    "sherpa-onnx-c-api.dll"
} else {
    "libsherpa-onnx-c-api.so"
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeSource {
    EnvVar,
    Bundled,
    LocalDir,
    Managed,
}

impl RuntimeSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::EnvVar => "环境变量覆盖",
            Self::Bundled => "应用包随附",
            Self::LocalDir => "程序同级目录",
            Self::Managed => "应用托管目录",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeStatus {
    Ready {
        path: PathBuf,
        source: RuntimeSource,
    },
    Missing(String),
}

impl RuntimeStatus {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimePackage {
    pub platform_label: &'static str,
    pub archive: &'static str,
    pub size_bytes: u64,
    pub sha256: Option<&'static str>,
}

impl RuntimePackage {
    pub fn download_url(&self) -> String {
        format!(
            "https://github.com/k2-fsa/sherpa-onnx/releases/download/v{RUNTIME_VERSION}/{}",
            self.archive
        )
    }
}

pub fn current_package() -> Option<RuntimePackage> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some(RuntimePackage {
            platform_label: "macOS Apple Silicon",
            archive: "sherpa-onnx-v2.0.2-osx-arm64-shared-lib.tar.bz2",
            size_bytes: 8_984_110,
            sha256: Some("1da59e61906f1c8dba86d80c799e34ef20eb89e90b49ea7d8bf7ef1257fb73ed"),
        }),
        ("macos", "x86_64") => Some(RuntimePackage {
            platform_label: "macOS Intel",
            archive: "sherpa-onnx-v2.0.2-osx-x64-shared-lib.tar.bz2",
            size_bytes: 10_273_709,
            sha256: Some("c1ff307855a9ee6fe2db862033eb3fefdf1b7950a253f364789b5397038102e4"),
        }),
        ("linux", "x86_64") => Some(RuntimePackage {
            platform_label: "Linux x64",
            archive: "sherpa-onnx-v2.0.2-linux-x64-shared-lib.tar.bz2",
            size_bytes: 10_049_208,
            sha256: None,
        }),
        ("linux", "aarch64") => Some(RuntimePackage {
            platform_label: "Linux ARM64",
            archive: "sherpa-onnx-v2.0.2-linux-aarch64-shared-cpu-lib.tar.bz2",
            size_bytes: 12_799_065,
            sha256: None,
        }),
        ("windows", "x86_64") => Some(RuntimePackage {
            platform_label: "Windows x64",
            archive: "sherpa-onnx-v2.0.2-win-x64-shared-MT-Release-lib.tar.bz2",
            size_bytes: 8_145_461,
            sha256: None,
        }),
        _ => None,
    }
}

pub fn managed_runtime_dir() -> PathBuf {
    crate::library::config_dir()
        .join("runtime")
        .join(format!("sherpa-onnx-{RUNTIME_VERSION}"))
}

pub fn current_runtime_status() -> RuntimeStatus {
    match std::env::current_exe() {
        Ok(executable) => detect_runtime(&executable),
        Err(error) => RuntimeStatus::Missing(format!("无法读取应用路径：{error}")),
    }
}

pub fn is_runtime_available() -> bool {
    current_runtime_status().is_ready()
}

pub fn detect_runtime(executable: &Path) -> RuntimeStatus {
    detect_runtime_in(executable, &managed_runtime_dir())
}

pub(crate) fn detect_runtime_in(executable: &Path, managed_dir: &Path) -> RuntimeStatus {
    detect_runtime_with_env(
        std::env::var_os("TFY_ASR_LIBRARY").filter(|value| !value.is_empty()),
        executable,
        managed_dir,
    )
}

fn detect_runtime_with_env(
    env_override: Option<std::ffi::OsString>,
    executable: &Path,
    managed_dir: &Path,
) -> RuntimeStatus {
    if let Some(value) = env_override {
        let path = PathBuf::from(value);
        return if path.is_absolute() && path.is_file() {
            RuntimeStatus::Ready {
                path,
                source: RuntimeSource::EnvVar,
            }
        } else {
            RuntimeStatus::Missing("TFY_ASR_LIBRARY 须指向本地 sherpa-onnx 动态库的绝对路径".into())
        };
    }
    let Some(program_dir) = executable.parent() else {
        return RuntimeStatus::Missing("找不到应用所在目录".into());
    };
    let bundled = program_dir
        .parent()
        .filter(|path| path.ends_with("Contents"))
        .map(|path| path.join("Resources/asr").join(ENGINE_FILE));
    if let Some(path) = bundled.filter(|path| path.is_file()) {
        return RuntimeStatus::Ready {
            path,
            source: RuntimeSource::Bundled,
        };
    }
    let local = program_dir.join("asr").join(ENGINE_FILE);
    if local.is_file() {
        return RuntimeStatus::Ready {
            path: local,
            source: RuntimeSource::LocalDir,
        };
    }
    let managed = managed_dir.join(ENGINE_FILE);
    if managed.is_file() {
        return RuntimeStatus::Ready {
            path: managed,
            source: RuntimeSource::Managed,
        };
    }
    RuntimeStatus::Missing("未找到 sherpa-onnx 2.0.2 推理库，可直接在应用内一键安装".into())
}

pub(super) fn runtime_library(executable: &Path) -> Result<PathBuf, String> {
    match detect_runtime(executable) {
        RuntimeStatus::Ready { path, .. } => Ok(path),
        RuntimeStatus::Missing(error) => Err(error),
    }
}

pub(super) fn open(runtime: &Path) -> Result<Library, String> {
    let library = unsafe { Library::new(runtime) }
        .map_err(|error| format!("无法加载 sherpa-onnx 动态库：{error}"))?;
    let version: unsafe extern "C" fn() -> *const c_char =
        unsafe { symbol(&library, b"SherpaOnnxGetVersionStr\0")? };
    let version = unsafe { version() };
    if version.is_null() || unsafe { CStr::from_ptr(version) }.to_bytes() != b"2.0.2" {
        return Err("当前 GPUI 需要 sherpa-onnx 2.0.2 的 C API 动态库".into());
    }
    Ok(library)
}

pub(super) unsafe fn symbol<T: Copy>(library: &Library, name: &[u8]) -> Result<T, String> {
    unsafe { library.get::<T>(name) }
        .map(|symbol| *symbol)
        .map_err(|error| format!("sherpa-onnx 缺少 C API 接口：{error}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeStage {
    Downloading,
    Verifying,
    Extracting,
    Checking,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeProgress {
    pub stage: RuntimeStage,
    pub received: u64,
    pub total: u64,
}

#[derive(Default)]
struct DownloadState {
    cancelled: AtomicBool,
    progress: Mutex<Option<RuntimeProgress>>,
}

#[derive(Default)]
pub struct RuntimeDownload(Arc<DownloadState>);

impl RuntimeDownload {
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn cancelling(&self) -> bool {
        self.0.cancelled.load(Ordering::Relaxed)
    }

    pub fn progress(&self) -> Option<RuntimeProgress> {
        self.0.progress.lock().ok()?.clone()
    }

    pub fn install_job(&self) -> impl FnOnce() -> Result<PathBuf, String> + use<> {
        let state = self.0.clone();
        move || {
            let package = current_package()
                .ok_or_else(|| "当前系统架构暂不支持自动安装，请使用导入本地包".to_owned())?;
            install_from_package(package, &managed_runtime_dir(), &state)
        }
    }

    pub fn install_from_local_job(
        &self,
        source: PathBuf,
    ) -> impl FnOnce() -> Result<PathBuf, String> + use<> {
        let state = self.0.clone();
        move || install_from_local(&source, &managed_runtime_dir(), &state)
    }
}

impl Drop for RuntimeDownload {
    fn drop(&mut self) {
        self.cancel();
    }
}

struct InstallLock(File);

impl Drop for InstallLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn update_progress(state: &DownloadState, stage: RuntimeStage, received: u64, total: u64) {
    if let Ok(mut guard) = state.progress.lock() {
        *guard = Some(RuntimeProgress {
            stage,
            received,
            total,
        });
    }
}

fn check_cancelled(state: &DownloadState) -> Result<(), String> {
    if state.cancelled.load(Ordering::Relaxed) {
        Err("已取消安装推理库".into())
    } else {
        Ok(())
    }
}

fn acquire_install_lock(target_dir: &Path) -> Result<InstallLock, String> {
    let parent = target_dir.parent().ok_or("推理库托管路径无效")?;
    fs::create_dir_all(parent).map_err(|error| format!("无法创建推理库目录：{error}"))?;
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(parent.join(".runtime-install.lock"))
        .map_err(|error| format!("无法创建安装锁：{error}"))?;
    lock_file
        .try_lock()
        .map_err(|error| format!("已有推理库安装任务正在进行：{error}"))?;
    Ok(InstallLock(lock_file))
}

fn install_from_package(
    package: RuntimePackage,
    target_dir: &Path,
    state: &DownloadState,
) -> Result<PathBuf, String> {
    let _lock = acquire_install_lock(target_dir)?;
    let parent = target_dir.parent().ok_or("推理库托管路径无效")?;
    let scratch = Builder::new()
        .prefix(".runtime-staging-")
        .tempdir_in(parent)
        .map_err(|error| format!("无法创建临时目录：{error}"))?;
    let archive = scratch.path().join(package.archive);

    update_progress(state, RuntimeStage::Downloading, 0, package.size_bytes);
    download_archive(
        Path::new(curl_binary()),
        &package.download_url(),
        &archive,
        package.size_bytes,
        state,
    )?;

    check_cancelled(state)?;
    if let Some(expected_hash) = package.sha256 {
        update_progress(
            state,
            RuntimeStage::Verifying,
            package.size_bytes,
            package.size_bytes,
        );
        verify_sha256(&archive, expected_hash)?;
    }

    install_from_archive_in(
        &archive,
        scratch.path(),
        target_dir,
        package.size_bytes,
        state,
    )
}

fn install_from_local(
    source: &Path,
    target_dir: &Path,
    state: &DownloadState,
) -> Result<PathBuf, String> {
    let _lock = acquire_install_lock(target_dir)?;
    let parent = target_dir.parent().ok_or("推理库托管路径无效")?;
    let scratch = Builder::new()
        .prefix(".runtime-local-")
        .tempdir_in(parent)
        .map_err(|error| format!("无法创建临时目录：{error}"))?;

    let meta = fs::metadata(source).map_err(|error| format!("无法读取所选路径：{error}"))?;
    if meta.is_file() {
        let lower = source
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if lower.ends_with(".tar.bz2")
            || lower.ends_with(".tbz2")
            || lower.ends_with(".tar.gz")
            || lower.ends_with(".tgz")
            || lower.ends_with(".tar")
        {
            return install_from_archive_in(source, scratch.path(), target_dir, meta.len(), state);
        }
        let source_dir = source.parent().ok_or("找不到动态库所在目录")?;
        return stage_and_publish_directory(source_dir, scratch.path(), target_dir, state);
    }
    if meta.is_dir() {
        return stage_and_publish_directory(source, scratch.path(), target_dir, state);
    }
    Err("请选择压缩包（.tar.bz2）或包含动态库的目录".into())
}

fn install_from_archive_in(
    archive: &Path,
    scratch: &Path,
    target_dir: &Path,
    total_bytes: u64,
    state: &DownloadState,
) -> Result<PathBuf, String> {
    check_cancelled(state)?;
    update_progress(state, RuntimeStage::Extracting, total_bytes, total_bytes);
    let extract_dir = scratch.join("extracted");
    fs::create_dir(&extract_dir).map_err(|error| format!("无法创建解压目录：{error}"))?;

    let status = Command::new(tar_binary())
        .args(["-xf"])
        .arg(archive)
        .arg("-C")
        .arg(&extract_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("无法启动解压程序：{error}"))?;
    if !status.status.success() {
        let detail = String::from_utf8_lossy(&status.stderr).trim().to_owned();
        return Err(if detail.is_empty() {
            "解压推理库压缩包失败".into()
        } else {
            format!("解压推理库压缩包失败：{detail}")
        });
    }

    stage_and_publish_directory(&extract_dir, scratch, target_dir, state)
}

fn stage_and_publish_directory(
    source_root: &Path,
    scratch: &Path,
    target_dir: &Path,
    state: &DownloadState,
) -> Result<PathBuf, String> {
    check_cancelled(state)?;
    let staged = scratch.join("staged");
    fs::create_dir(&staged).map_err(|error| format!("无法创建安装暂存目录：{error}"))?;

    let mut copied = 0usize;
    collect_dynamic_libraries(source_root, &staged, 0, &mut copied)?;
    if copied == 0 {
        return Err(format!("所选内容中未找到 {ENGINE_FILE} 及关联动态库"));
    }

    let engine = staged.join(ENGINE_FILE);
    if !engine.is_file() {
        return Err(format!("缺少核心动态库 {ENGINE_FILE}"));
    }

    check_cancelled(state)?;
    update_progress(state, RuntimeStage::Checking, 1, 1);
    drop(open(&engine)?);

    publish_runtime(&staged, target_dir)?;
    Ok(target_dir.join(ENGINE_FILE))
}

fn collect_dynamic_libraries(
    dir: &Path,
    staged: &Path,
    depth: usize,
    copied: &mut usize,
) -> Result<(), String> {
    if depth > 4 {
        return Ok(());
    }
    let entries = fs::read_dir(dir).map_err(|error| format!("无法扫描动态库目录：{error}"))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("读取目录项失败：{error}"))?;
        let path = entry.path();
        let meta =
            fs::symlink_metadata(&path).map_err(|error| format!("读取文件信息失败：{error}"))?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            collect_dynamic_libraries(&path, staged, depth + 1, copied)?;
            continue;
        }
        if !meta.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !is_runtime_shared_library(name) {
            continue;
        }
        let dest = staged.join(name);
        fs::copy(&path, &dest).map_err(|error| format!("复制 {name} 失败：{error}"))?;
        set_executable_permissions(&dest);
        *copied += 1;
    }
    Ok(())
}

fn is_runtime_shared_library(name: &str) -> bool {
    if cfg!(target_os = "macos") {
        name.ends_with(".dylib")
    } else if cfg!(target_os = "windows") {
        name.to_ascii_lowercase().ends_with(".dll")
    } else {
        name == "libsherpa-onnx-c-api.so" || name.ends_with(".so") || name.contains(".so.")
    }
}

#[cfg(unix)]
fn set_executable_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o755);
        let _ = fs::set_permissions(path, perms);
    }
}

#[cfg(not(unix))]
fn set_executable_permissions(_path: &Path) {}

fn publish_runtime(staged: &Path, target_dir: &Path) -> Result<(), String> {
    let parent = target_dir.parent().ok_or("推理库托管路径无效")?;
    let backup = Builder::new()
        .prefix(".runtime-backup-")
        .tempdir_in(parent)
        .map_err(|error| format!("无法准备替换目录：{error}"))?;
    let backup_slot = backup.path().join("previous");
    let had_existing = fs::symlink_metadata(target_dir).is_ok();
    if had_existing {
        fs::rename(target_dir, &backup_slot)
            .map_err(|error| format!("无法替换旧版推理库：{error}"))?;
    }
    if let Err(error) = fs::rename(staged, target_dir) {
        if had_existing {
            let _ = fs::rename(&backup_slot, target_dir);
        }
        return Err(format!("写入推理库目录失败：{error}"));
    }
    Ok(())
}

fn verify_sha256(path: &Path, expected_hex: &str) -> Result<(), String> {
    let mut file = File::open(path).map_err(|error| format!("无法读取安装包校验摘要：{error}"))?;
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buf)
            .map_err(|error| format!("读取安装包失败：{error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    let actual = format!("{:x}", hasher.finalize());
    if actual.eq_ignore_ascii_case(expected_hex) {
        Ok(())
    } else {
        Err("下载的安装包 SHA-256 校验未通过，请重试或改用导入本地包".into())
    }
}

fn download_archive(
    curl: &Path,
    url: &str,
    dest: &Path,
    total_bytes: u64,
    state: &DownloadState,
) -> Result<(), String> {
    let mut cmd = Command::new(curl);
    cmd.args([
        "-fL",
        "--connect-timeout",
        DOWNLOAD_CONNECT_TIMEOUT_SECS,
        "--retry",
        "2",
        "--silent",
        "--show-error",
        "-o",
    ])
    .arg(dest)
    .arg(url)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|error| format!("无法启动下载器（curl）：{error}"))?;

    let outcome = wait_for_download(&mut child, dest, total_bytes, state);
    if outcome.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    outcome
}

fn wait_for_download(
    child: &mut Child,
    dest: &Path,
    total_bytes: u64,
    state: &DownloadState,
) -> Result<(), String> {
    let started = Instant::now();
    let mut last_progress_at = started;
    let mut last_bytes = 0u64;

    loop {
        if let Err(err) = check_cancelled(state) {
            return Err(err);
        }
        let current_bytes = fs::metadata(dest).map_or(0, |meta| meta.len());
        if current_bytes > last_bytes {
            last_bytes = current_bytes;
            last_progress_at = Instant::now();
        }
        update_progress(
            state,
            RuntimeStage::Downloading,
            current_bytes.min(total_bytes),
            total_bytes,
        );
        match child
            .try_wait()
            .map_err(|error| format!("检查下载状态失败：{error}"))?
        {
            Some(status) if status.success() => return Ok(()),
            Some(_) => {
                let mut stderr_buf = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    let _ = io::Read::read_to_string(&mut stderr, &mut stderr_buf);
                }
                let detail = stderr_buf.trim();
                return Err(if detail.is_empty() {
                    "下载推理库失败，请检查网络连接或改用导入本地包".into()
                } else {
                    format!("下载推理库失败：{detail}")
                });
            }
            None => {
                if last_progress_at.elapsed() >= DOWNLOAD_STALL_TIMEOUT {
                    return Err("下载推理库长时间无响应，请重试或改用导入本地包".into());
                }
                if started.elapsed() >= DOWNLOAD_TOTAL_TIMEOUT {
                    return Err("下载推理库超时，请重试或改用导入本地包".into());
                }
                std::thread::sleep(Duration::from_millis(80));
            }
        }
    }
}

fn curl_binary() -> &'static str {
    if cfg!(target_os = "macos") {
        "/usr/bin/curl"
    } else {
        "curl"
    }
}

fn tar_binary() -> &'static str {
    if cfg!(target_os = "macos") {
        "/usr/bin/tar"
    } else {
        "tar"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_runtime_with_priority_and_falls_back_to_managed_dir() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("TFY-BGM.app/Contents/MacOS/tfy-bgm-gpui");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, b"").unwrap();

        let managed = dir.path().join("config/runtime/sherpa-onnx-2.0.2");
        fs::create_dir_all(&managed).unwrap();
        let managed_lib = managed.join(ENGINE_FILE);
        fs::write(&managed_lib, b"managed").unwrap();

        let status = detect_runtime_with_env(None, &exe, &managed);
        assert_eq!(
            status,
            RuntimeStatus::Ready {
                path: managed_lib.clone(),
                source: RuntimeSource::Managed,
            }
        );

        let local = exe.parent().unwrap().join("asr").join(ENGINE_FILE);
        fs::create_dir_all(local.parent().unwrap()).unwrap();
        fs::write(&local, b"local").unwrap();
        assert_eq!(
            detect_runtime_with_env(None, &exe, &managed),
            RuntimeStatus::Ready {
                path: local,
                source: RuntimeSource::LocalDir,
            }
        );

        let bundled = dir
            .path()
            .join("TFY-BGM.app/Contents/Resources/asr")
            .join(ENGINE_FILE);
        fs::create_dir_all(bundled.parent().unwrap()).unwrap();
        fs::write(&bundled, b"bundled").unwrap();
        assert_eq!(
            detect_runtime_with_env(None, &exe, &managed),
            RuntimeStatus::Ready {
                path: bundled,
                source: RuntimeSource::Bundled,
            }
        );

        let env_lib = dir.path().join("override").join(ENGINE_FILE);
        fs::create_dir_all(env_lib.parent().unwrap()).unwrap();
        fs::write(&env_lib, b"env").unwrap();
        assert_eq!(
            detect_runtime_with_env(Some(env_lib.clone().into_os_string()), &exe, &managed),
            RuntimeStatus::Ready {
                path: env_lib,
                source: RuntimeSource::EnvVar,
            }
        );
    }

    #[test]
    fn rejects_invalid_sha256_and_missing_engine_in_local_dir() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("archive.bin");
        fs::write(&file, b"fixture").unwrap();
        let err = verify_sha256(&file, "0".repeat(64).as_str()).unwrap_err();
        assert!(err.contains("SHA-256"));

        let empty_dir = dir.path().join("empty");
        fs::create_dir_all(&empty_dir).unwrap();
        let target = dir.path().join("managed/runtime");
        let state = DownloadState::default();
        let missing = install_from_local(&empty_dir, &target, &state).unwrap_err();
        assert!(missing.contains(ENGINE_FILE));
    }

    #[test]
    fn publish_runtime_replaces_existing_directory_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("runtime/sherpa-onnx-2.0.2");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("old.txt"), b"old").unwrap();

        let staged = dir.path().join("runtime/.staged");
        fs::create_dir_all(&staged).unwrap();
        fs::write(staged.join(ENGINE_FILE), b"new").unwrap();

        publish_runtime(&staged, &target).unwrap();
        assert!(!target.join("old.txt").exists());
        assert_eq!(fs::read(target.join(ENGINE_FILE)).unwrap(), b"new");
    }
}
