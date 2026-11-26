//! 模型目录：路径解析、目录扫描与默认模型状态。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use bgm_core::Prefs;
use bgm_core::paths::MODELS_DIR_NAME;

use super::{AsrModel, COMPONENTS, DEFAULT_MODEL_NAME, DefaultModel, MAX_SCAN_DEPTH, QWEN3_FILES};

/// 默认模型目录：系统「下载」目录下的 `bgm-models`。
///
/// Windows 取 `FOLDERID_Downloads`（用户重定向过的下载目录同样有效），
/// Linux 取 `XDG_DOWNLOAD_DIR`；两者通常都不在程序所在盘，避免大体积模型
/// 占满软件安装分区。系统未定义下载目录时退到 `<用户主目录>/Downloads`。
pub fn default_root() -> PathBuf {
    let base = dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join("Downloads")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join(MODELS_DIR_NAME)
}

/// 当前生效的模型目录：用户自定义优先，否则回退默认位置。
pub fn root_for(prefs: &Prefs) -> PathBuf {
    prefs
        .models_dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(default_root)
}

/// 目录不存在时创建；返回的错误由调用方决定是否作为告警展示。
pub fn ensure_root(root: &Path) -> Result<()> {
    fs::create_dir_all(root).with_context(|| format!("无法创建模型目录：{}", root.display()))
}

/// 扫描模型目录下所有 `.onnx`，按所在目录聚合成 ASR 模型列表。
///
/// 目录内的多个 `.onnx`（如 Qwen3 的 conv_frontend / encoder / decoder）属于同一个
/// 模型；直接放在模型根目录下的 `.onnx` 视为单文件模型，各自成条。
pub fn scan(root: &Path) -> (Vec<AsrModel>, Vec<String>) {
    let mut warnings = Vec::new();
    if !root.is_dir() {
        return (
            Vec::new(),
            vec![format!("模型目录不存在：{}", root.display())],
        );
    }

    let mut onnx_files = Vec::new();
    collect_onnx(root, 0, &mut onnx_files, &mut warnings);

    let mut groups: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for path in onnx_files {
        let parent = path.parent().unwrap_or(root).to_path_buf();
        groups.entry(parent).or_default().push(path);
    }

    let mut models = Vec::with_capacity(groups.len());
    for (dir, mut files) in groups {
        files.sort();
        let relative = dir.strip_prefix(root).unwrap_or(Path::new(""));
        let is_root = relative.as_os_str().is_empty();
        let file_names: Vec<String> = files
            .iter()
            .filter_map(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect();

        if is_root {
            // 根目录下的单文件模型：文件名即模型名，不满足 Qwen3 目录结构。
            for (path, file_name) in files.iter().zip(&file_names) {
                let name = path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_else(|| file_name.clone());
                models.push(AsrModel {
                    id: format!("file:{name}"),
                    name,
                    path: root.to_string_lossy().into_owned(),
                    files: vec![file_name.clone()],
                    size_bytes: fs::metadata(path).map(|meta| meta.len()).unwrap_or(0),
                    is_default: false,
                    loadable: false,
                    // 单文件模型本来就不满足 Qwen3 目录结构，逐项列出缺失构件只会误导。
                    missing: Vec::new(),
                });
            }
            continue;
        }

        let name = dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| relative.to_string_lossy().replace('\\', "/"));
        let missing = missing_components(&dir);
        models.push(AsrModel {
            id: relative.to_string_lossy().replace('\\', "/"),
            name,
            path: dir.to_string_lossy().into_owned(),
            files: file_names,
            size_bytes: dir_size(&dir),
            is_default: relative == Path::new(DEFAULT_MODEL_NAME),
            loadable: missing.is_empty(),
            missing,
        });
    }

    models.sort_by(|left, right| {
        right
            .is_default
            .cmp(&left.is_default)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    (models, warnings)
}

/// 按扫描结果解析并校验删除目标；与真正删除分开，便于运行时先卸载再移除文件。
///
/// 目录模型返回目录本身，根目录下的单文件模型返回该文件；两者都做 canonical 校验，
/// 防止模型目录或目标文件被替换为指向外部的符号链接。真正的删除由
/// [`delete_model_files`] 完成。
pub(super) fn resolve_scanned_model(root: &Path, id: &str) -> Result<(AsrModel, PathBuf)> {
    let (models, _) = scan(root);
    let model = models
        .into_iter()
        .find(|model| model.id == id)
        .ok_or_else(|| anyhow!("模型不存在或已被删除：{id}"))?;
    let canonical_root =
        fs::canonicalize(root).with_context(|| format!("模型目录不存在：{}", root.display()))?;
    let target = if model.id.starts_with("file:") {
        root.join(&model.files[0])
    } else {
        PathBuf::from(&model.path)
    };
    let canonical_target =
        fs::canonicalize(&target).with_context(|| format!("模型不存在：{}", target.display()))?;
    if canonical_target == canonical_root || !canonical_target.starts_with(&canonical_root) {
        bail!("模型必须位于当前模型目录下：{}", root.display());
    }
    Ok((model, target))
}

pub(super) fn delete_model_files(model: &AsrModel, target: &Path) -> Result<()> {
    if model.id.starts_with("file:") {
        fs::remove_file(target)
            .with_context(|| format!("删除模型文件失败：{}", target.display()))?;
    } else {
        fs::remove_dir_all(target)
            .with_context(|| format!("删除模型目录失败：{}", target.display()))?;
    }
    Ok(())
}

/// 删除前记录模型文件清单；失败时据此判断能否安全恢复已卸载的模型句柄。
pub(super) fn snapshot_model_files(
    model: &AsrModel,
    target: &Path,
) -> Result<BTreeMap<PathBuf, u64>> {
    let mut files = BTreeMap::new();
    if model.id.starts_with("file:") {
        let metadata = fs::metadata(target)
            .with_context(|| format!("无法读取模型文件：{}", target.display()))?;
        if !metadata.is_file() {
            bail!("模型目标不是文件：{}", target.display());
        }
        files.insert(target.to_path_buf(), metadata.len());
    } else {
        collect_file_sizes(target, &mut files)?;
    }
    Ok(files)
}

pub(super) fn collect_file_sizes(dir: &Path, files: &mut BTreeMap<PathBuf, u64>) -> Result<()> {
    let entries =
        fs::read_dir(dir).with_context(|| format!("无法读取模型目录：{}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("无法读取模型目录项：{}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("无法读取模型文件类型：{}", path.display()))?;
        if file_type.is_dir() {
            collect_file_sizes(&path, files)?;
        } else {
            let metadata = entry
                .metadata()
                .with_context(|| format!("无法读取模型文件：{}", path.display()))?;
            files.insert(path, metadata.len());
        }
    }
    Ok(())
}

/// 默认模型的安装状态；目录存在但构件不全时同样视为未安装，下载会续传缺失文件。
pub fn default_model(root: &Path) -> DefaultModel {
    let dir = root.join(DEFAULT_MODEL_NAME);
    let exists = dir.is_dir();
    let missing = if exists {
        missing_components(&dir)
    } else {
        COMPONENTS.iter().map(|name| name.to_string()).collect()
    };
    DefaultModel {
        name: DEFAULT_MODEL_NAME.to_owned(),
        path: dir.to_string_lossy().into_owned(),
        installed: exists && missing.is_empty(),
        missing,
        size_bytes: if exists { dir_size(&dir) } else { 0 },
        expected_bytes: QWEN3_FILES.iter().map(|file| file.bytes).sum(),
    }
}

/// 已加载模型与下载任务的运行期状态；克隆只复制内部共享句柄。
pub(super) fn total_bytes() -> u64 {
    QWEN3_FILES.iter().map(|file| file.bytes).sum()
}

pub fn is_loadable(dir: &Path) -> bool {
    dir.is_dir() && missing_components(dir).is_empty()
}

/// Qwen3 ASR 四件套中缺失的项；`tokenizer` 单独按目录判断。
pub(super) fn missing_components(dir: &Path) -> Vec<String> {
    let mut missing = Vec::new();
    for name in [
        "conv_frontend.onnx",
        "encoder.int8.onnx",
        "decoder.int8.onnx",
    ] {
        if !dir.join(name).is_file() {
            missing.push(name.to_owned());
        }
    }
    if !dir.join("tokenizer").is_dir() {
        missing.push("tokenizer/".to_owned());
    }
    missing
}

pub(super) fn collect_onnx(
    dir: &Path,
    depth: usize,
    out: &mut Vec<PathBuf>,
    warnings: &mut Vec<String>,
) {
    if depth > MAX_SCAN_DEPTH {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(format!("无法读取目录 {}：{error}", dir.display()));
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        // 不跟随符号链接，避免自引用目录造成无限扫描。
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            let name = entry.file_name();
            let hidden = name.to_string_lossy().starts_with('.');
            let other_category = depth == 0
                && (name == crate::separation::CUSTOM_DIR_NAME || name == crate::tts_models::DIR);
            if !hidden && !other_category {
                collect_onnx(&path, depth + 1, out, warnings);
            }
        } else if is_onnx(&path) {
            out.push(path);
        }
    }
}

pub(super) fn is_onnx(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("onnx"))
}

pub(super) fn dir_size(dir: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = fs::read_dir(dir) else {
        return total;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            total += dir_size(&path);
        } else if let Ok(meta) = entry.metadata() {
            total += meta.len();
        }
    }
    total
}
