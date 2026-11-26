//! 分离模型目录：随包模型、自定义模型、下载包解压与兼容性预检。

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use tauri::path::BaseDirectory;
use tauri::{AppHandle, Manager};

use super::{
    ACCOMPANIMENT_MODEL_FILE, ACCOMPANIMENT_MODEL_ID, CUSTOM_DIR_NAME, SPLEETER_ARCHIVE,
    SPLEETER_DIR, SeparationCatalog, SeparationModelInfo, Source, Spec, StemTarget,
};

/// 随包模型目录：优先 Tauri 资源目录，开发时回退到仓库 `models/`。
fn bundled_dir(app: &AppHandle) -> Option<PathBuf> {
    let resource = app
        .path()
        .resolve("models", BaseDirectory::Resource)
        .ok()
        .filter(|path| path.is_dir());
    if resource.is_some() {
        return resource;
    }
    #[cfg(debug_assertions)]
    {
        let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../models")
            .canonicalize()
            .ok()
            .filter(|path| path.is_dir());
        if dev.is_some() {
            return dev;
        }
    }
    None
}

fn extracted_spleeter_dir(app: &AppHandle) -> Result<PathBuf> {
    let dir = app
        .path()
        .app_data_dir()
        .context("无法定位应用数据目录")?
        .join("separation-models")
        .join(SPLEETER_DIR);
    fs::create_dir_all(&dir)
        .with_context(|| format!("无法创建 spleeter 模型目录：{}", dir.display()))?;
    Ok(dir)
}

fn find_named_onnx(dir: &Path, needle: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    let mut found = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
        if name.ends_with(".onnx") && name.contains(needle) {
            // 名称完全相同的候选按字典序取第一个，保证多次运行结果稳定。
            if found
                .as_ref()
                .is_none_or(|current: &PathBuf| path < *current)
            {
                found = Some(path);
            }
        }
    }
    found
}

pub(super) fn spleeter_files(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let vocals = find_named_onnx(dir, "vocals")?;
    let accompaniment = find_named_onnx(dir, "accompaniment")?;
    Some((vocals, accompaniment))
}

fn spleeter_spec(app: &AppHandle, bundled: Option<&Path>) -> Spec {
    let extracted = extracted_spleeter_dir(app).ok();
    let ready = extracted.as_deref().and_then(spleeter_files);
    let archive = bundled.map(|dir| dir.join(SPLEETER_ARCHIVE));
    let archive_exists = archive.as_deref().is_some_and(Path::is_file);
    let (source, detail) = match ready {
        Some((vocals, accompaniment)) => (
            Some(Source::Spleeter {
                vocals,
                accompaniment,
            }),
            "已就绪（随包模型）".to_owned(),
        ),
        None if archive_exists => (None, "随包模型，首次使用时会自动解压".to_owned()),
        None => (None, format!("缺少模型文件：{SPLEETER_ARCHIVE}")),
    };
    Spec {
        id: "spleeter-2stems-int8".to_owned(),
        name: "sherpa-onnx-spleeter-2stems-int8".to_owned(),
        engine: "Spleeter 2stems".to_owned(),
        target: StemTarget::Vocals,
        target_known: true,
        description: "双轨分离（人声 + 伴奏），速度较快，适合人声清晰的歌曲。".to_owned(),
        source,
        installable: archive_exists,
        detail,
    }
}

fn uvr_spec(bundled: Option<&Path>, file_name: &str, id: &str, target: StemTarget) -> Spec {
    let path = bundled.map(|dir| dir.join(file_name));
    let available = path.as_deref().is_some_and(Path::is_file);
    let description = match target {
        StemTarget::Vocals => "UVR 人声分离：模型直接输出人声，伴奏由混合音轨减人声得到。",
        StemTarget::Instrumental => "UVR 乐器分离：模型输出乐器轨，人声由混合音轨减乐器得到。",
    };
    Spec {
        id: id.to_owned(),
        name: file_name.trim_end_matches(".onnx").to_owned(),
        engine: "UVR MDX-Net".to_owned(),
        target,
        target_known: true,
        description: description.to_owned(),
        source: available.then(|| Source::Uvr {
            model: path.unwrap(),
        }),
        installable: false,
        detail: if available {
            file_name.to_owned()
        } else {
            format!("缺少模型文件：{file_name}")
        },
    }
}

pub(super) fn uvr_target_from_name(name: &str) -> StemTarget {
    let lower = name.to_ascii_lowercase();
    if lower.contains("inst") {
        StemTarget::Instrumental
    } else {
        StemTarget::Vocals
    }
}

/// 扫描 `<模型目录>/separation/` 下的自定义模型。
///
/// - 目录内同时有 `vocals*.onnx` 与 `accompaniment*.onnx` → Spleeter 双模型；
/// - `separation/` 根目录下的其余 `.onnx` → UVR 模型，文件名含 `Inst` 时按乐器模型处理。
fn custom_specs(root: &Path) -> Vec<Spec> {
    let dir = root.join(CUSTOM_DIR_NAME);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut specs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if let Some((vocals, accompaniment)) = spleeter_files(&path) {
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "自定义 Spleeter".to_owned());
                let relative = path
                    .strip_prefix(&dir)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                specs.push(Spec {
                    id: format!("custom:{relative}"),
                    name: name.clone(),
                    engine: "Spleeter 2stems".to_owned(),
                    target: StemTarget::Vocals,
                    target_known: true,
                    description: "自定义双轨分离模型（vocals + accompaniment）。".to_owned(),
                    source: Some(Source::Spleeter {
                        vocals,
                        accompaniment,
                    }),
                    installable: false,
                    detail: path.to_string_lossy().into_owned(),
                });
                continue;
            }
            continue;
        }
        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("onnx"))
        {
            let name = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| "自定义 UVR".to_owned());
            let relative = path
                .strip_prefix(&dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            specs.push(Spec {
                id: format!("custom:{relative}"),
                name: name.clone(),
                engine: "UVR MDX-Net".to_owned(),
                target: uvr_target_from_name(&name),
                // 自定义 UVR 模型没有角色元数据，文件名只是猜测；生成前让用户确认。
                target_known: false,
                description: "自定义 UVR 模型；必须是 sherpa-onnx 兼容的 2-stem ONNX 模型。"
                    .to_owned(),
                source: Some(Source::Uvr {
                    model: path.clone(),
                }),
                installable: false,
                detail: path.to_string_lossy().into_owned(),
            });
        }
    }
    specs.sort_by_key(|spec| spec.name.to_lowercase());
    specs
}

fn specs(app: &AppHandle, models_root: &Path) -> Vec<Spec> {
    let bundled = bundled_dir(app);
    let mut list = vec![
        spleeter_spec(app, bundled.as_deref()),
        uvr_spec(
            bundled.as_deref(),
            ACCOMPANIMENT_MODEL_FILE,
            ACCOMPANIMENT_MODEL_ID,
            StemTarget::Instrumental,
        ),
        uvr_spec(
            bundled.as_deref(),
            "UVR-MDX-NET-Voc_FT.onnx",
            "uvr-voc-ft",
            StemTarget::Vocals,
        ),
    ];
    list.extend(custom_specs(models_root));
    list
}

pub(super) fn target_label(target: StemTarget) -> &'static str {
    match target {
        StemTarget::Vocals => "vocals",
        StemTarget::Instrumental => "instrumental",
    }
}

impl Spec {
    /// 转成前端展示项；随包模型即使缺失也保留一条，界面据此显示缺失原因。
    fn info(&self) -> SeparationModelInfo {
        let available = self.installable || self.source.as_ref().is_some_and(source_exists);
        let detail = if available || self.detail.starts_with("缺少") {
            self.detail.clone()
        } else {
            format!("{}；首次使用时会自动解压", self.detail)
        };
        SeparationModelInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            engine: self.engine.clone(),
            target: target_label(self.target).to_owned(),
            target_known: self.target_known,
            description: self.description.clone(),
            available,
            detail,
        }
    }
}

/// 分离模型目录：人声模型由用户选择，伴奏固定为 UVR-MDX-NET-Inst_HQ_3。
pub fn catalog(app: &AppHandle, models_root: &Path) -> SeparationCatalog {
    let mut specs = specs(app, models_root);
    let accompaniment = specs
        .iter()
        .find(|spec| spec.id == ACCOMPANIMENT_MODEL_ID)
        .map(Spec::info)
        .expect("随包伴奏模型条目必须存在");
    // 人声列表只保留人声主输出的模型；伴奏固定由内置乐器模型生成，不在此列出。
    specs.retain(|spec| spec.id != ACCOMPANIMENT_MODEL_ID && spec.target == StemTarget::Vocals);
    SeparationCatalog {
        vocals: specs.iter().map(Spec::info).collect(),
        accompaniment,
    }
}

pub(super) fn source_exists(source: &Source) -> bool {
    match source {
        Source::Spleeter {
            vocals,
            accompaniment,
        } => vocals.is_file() && accompaniment.is_file(),
        Source::Uvr { model } => model.is_file(),
    }
}

/// 找到模型并确保 spleeter 已解压；返回可直接送入引擎的模型文件。
pub(super) fn resolve_spec(app: &AppHandle, models_root: &Path, id: &str) -> Result<Spec> {
    let mut spec = specs(app, models_root)
        .into_iter()
        .find(|spec| spec.id == id)
        .ok_or_else(|| anyhow!("所选人声分离模型不存在：{id}。请重新选择模型"))?;
    if spec.source.is_none() && spec.installable {
        let bundled = bundled_dir(app);
        let archive = bundled
            .as_deref()
            .map(|dir| dir.join(SPLEETER_ARCHIVE))
            .filter(|path| path.is_file())
            .ok_or_else(|| anyhow!("分离模型不可用：{}（{}）", spec.name, spec.detail))?;
        let target = extracted_spleeter_dir(app)?;
        extract_archive(&archive, &target)?;
        let (vocals, accompaniment) = spleeter_files(&target)
            .ok_or_else(|| anyhow!("解压后未找到 spleeter 模型文件：{}", target.display()))?;
        spec.source = Some(Source::Spleeter {
            vocals,
            accompaniment,
        });
        spec.detail = "已就绪（随包模型）".to_owned();
        spec.installable = false;
    }
    if spec.source.is_none() {
        bail!("分离模型不可用：{}（{}）", spec.name, spec.detail);
    }
    Ok(spec)
}

/// 解析固定的伴奏模型；缺失时返回可读错误，不静默降级。
pub(super) fn resolve_accompaniment(app: &AppHandle, models_root: &Path) -> Result<Spec> {
    let spec = specs(app, models_root)
        .into_iter()
        .find(|spec| spec.id == ACCOMPANIMENT_MODEL_ID)
        .ok_or_else(|| anyhow!("缺少固定的伴奏分离模型：{ACCOMPANIMENT_MODEL_FILE}"))?;
    if spec
        .source
        .as_ref()
        .is_none_or(|source| !source_exists(source))
    {
        bail!("伴奏分离模型不可用：{}（{}）", spec.name, spec.detail);
    }
    Ok(spec)
}

/// 解压随包模型；先解到临时目录再整体改名，避免中断留下半截模型。
pub(super) fn extract_archive(archive: &Path, target: &Path) -> Result<()> {
    if spleeter_files(target).is_some() {
        return Ok(());
    }
    let part = target.with_extension("part");
    let _ = fs::remove_dir_all(&part);
    fs::create_dir_all(&part).with_context(|| format!("无法创建解压目录：{}", part.display()))?;
    let file = File::open(archive)
        .with_context(|| format!("无法读取分离模型压缩包：{}", archive.display()))?;
    let decoder = bzip2::read::BzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);
    tar.unpack(&part)
        .with_context(|| format!("解压分离模型失败：{}", archive.display()))?;
    let nested = part.join(SPLEETER_DIR);
    let extracted = if nested.is_dir() {
        nested
    } else {
        part.clone()
    };
    if spleeter_files(&extracted).is_none() {
        let _ = fs::remove_dir_all(&part);
        bail!("解压后的压缩包内未找到 vocals / accompaniment 模型");
    }
    if target.exists() {
        fs::remove_dir_all(target)
            .with_context(|| format!("无法替换旧模型目录：{}", target.display()))?;
    }
    if extracted == part {
        fs::rename(&part, target)
            .with_context(|| format!("无法保存 spleeter 模型：{}", target.display()))?;
    } else {
        fs::rename(&extracted, target)
            .with_context(|| format!("无法保存 spleeter 模型：{}", target.display()))?;
        let _ = fs::remove_dir_all(&part);
    }
    Ok(())
}

/// sherpa-onnx 对 UVR 模型要求的全部元数据键；缺失任一项会 `_Exit(-1)`。
const UVR_REQUIRED_METADATA: &[&str] = &[
    "sample_rate",
    "n_fft",
    "center",
    "win_length",
    "hop_length",
    "dim_t",
    "dim_f",
    "dim_c",
    "window_type",
];

/// 流式扫描 ONNX 元数据，避免把整个模型（几十 MB）读进内存。
///
/// ONNX 的 `metadata_props` 是 `StringStringEntryProto`（key=1, value=2），
/// 在未压缩的 protobuf 字节流里表现为 `0x0a <len> key 0x12 <len> value`。
fn scan_metadata(path: &Path) -> Result<HashMap<String, String>> {
    const CHUNK: usize = 1 << 20;
    /// 单条元数据远小于该值；保留尾部以处理跨块条目。
    const OVERLAP: usize = 8 << 10;
    const KEYS: &[&str] = &[
        "model_type",
        "model_name",
        "sample_rate",
        "n_fft",
        "center",
        "win_length",
        "hop_length",
        "dim_t",
        "dim_f",
        "dim_c",
        "window_type",
        "stems",
    ];

    fn collect(buffer: &[u8], map: &mut HashMap<String, String>) {
        let mut index = 0;
        while index + 2 <= buffer.len() {
            if buffer[index] == 0x0a {
                let key_len = buffer[index + 1] as usize;
                let key_end = index + 2 + key_len;
                if key_len > 0 && key_end + 2 <= buffer.len() && buffer[key_end] == 0x12 {
                    let value_len = buffer[key_end + 1] as usize;
                    let value_start = key_end + 2;
                    if value_start + value_len <= buffer.len()
                        && let (Ok(key), Ok(value)) = (
                            std::str::from_utf8(&buffer[index + 2..key_end]),
                            std::str::from_utf8(&buffer[value_start..value_start + value_len]),
                        )
                        && KEYS.contains(&key)
                    {
                        map.insert(key.to_owned(), value.to_owned());
                    }
                }
            }
            index += 1;
        }
    }

    let file = File::open(path).with_context(|| format!("无法读取分离模型：{}", path.display()))?;
    let mut reader = BufReader::with_capacity(CHUNK, file);
    let mut chunk = vec![0u8; CHUNK];
    let mut buffer: Vec<u8> = Vec::with_capacity(CHUNK + OVERLAP);
    let mut map = HashMap::new();
    loop {
        let read = reader
            .read(&mut chunk)
            .with_context(|| format!("无法读取分离模型：{}", path.display()))?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        collect(&buffer, &mut map);
        if buffer.len() > OVERLAP {
            buffer.drain(..buffer.len() - OVERLAP);
        }
    }
    Ok(map)
}

/// 自定义模型进入 sherpa-onnx 前先校验元数据。
///
/// sherpa-onnx 遇到不兼容模型会直接 `_Exit(-1)`；虽然分离已放到子进程执行，
/// 这里仍做前置校验，把「模型不兼容」变成可读错误而不是子进程异常退出。
pub(super) fn preflight_custom(source: &Source) -> Result<()> {
    fn inspect(path: &Path, model_type: &str, required: &[&str]) -> Result<()> {
        let metadata = scan_metadata(path)?;
        let actual = metadata.get("model_type").map(String::as_str).unwrap_or("");
        if actual != model_type {
            bail!(
                "模型 {} 不是 sherpa-onnx 兼容的 {model_type} 模型（model_type={actual:?}），已拒绝加载",
                path.display()
            );
        }
        let stems = metadata
            .get("stems")
            .and_then(|value| value.parse::<i32>().ok())
            .unwrap_or(0);
        if stems != 2 {
            bail!(
                "模型 {} 不是 2-stem 模型（stems={stems}），已拒绝加载",
                path.display()
            );
        }
        for key in required {
            if !metadata.contains_key(*key) {
                bail!(
                    "模型 {} 缺少 sherpa-onnx 必需的元数据 `{key}`，已拒绝加载",
                    path.display()
                );
            }
        }
        Ok(())
    }
    match source {
        Source::Spleeter {
            vocals,
            accompaniment,
        } => {
            inspect(vocals, "spleeter", &[])?;
            inspect(accompaniment, "spleeter", &[])?;
        }
        Source::Uvr { model } => inspect(model, "UVR", UVR_REQUIRED_METADATA)?,
    }
    Ok(())
}
