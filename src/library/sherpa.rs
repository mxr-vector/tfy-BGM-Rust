use std::ffi::{CStr, c_char};
use std::path::{Path, PathBuf};

use libloading::Library;

const ENGINE_FILE: &str = if cfg!(target_os = "macos") {
    "libsherpa-onnx-c-api.dylib"
} else if cfg!(target_os = "windows") {
    "sherpa-onnx-c-api.dll"
} else {
    "libsherpa-onnx-c-api.so"
};

pub(super) fn runtime_library(executable: &Path) -> Result<PathBuf, String> {
    if let Some(value) = std::env::var_os("TFY_ASR_LIBRARY").filter(|value| !value.is_empty()) {
        let path = PathBuf::from(value);
        return (path.is_absolute() && path.is_file())
            .then_some(path)
            .ok_or_else(|| "TFY_ASR_LIBRARY 须指向本地 sherpa-onnx 动态库的绝对路径".into());
    }
    let program_dir = executable.parent().ok_or("找不到应用所在目录")?;
    let path = program_dir.join("asr").join(ENGINE_FILE);
    let bundled = program_dir
        .parent()
        .filter(|path| path.ends_with("Contents"))
        .map(|path| path.join("Resources/asr").join(ENGINE_FILE));
    bundled
        .into_iter()
        .chain([path])
        .find(|path| path.is_file())
        .ok_or_else(|| "未找到 sherpa-onnx 2.0.2 推理库，模型文件下载与推理库分开安装".into())
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
