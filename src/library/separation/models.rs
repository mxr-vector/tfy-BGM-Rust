use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

pub const ACCOMPANIMENT: &str = "UVR-MDX-NET-Inst_HQ_3";
const VOCALS: &str = "UVR-MDX-NET-Voc_FT";
const SPLEETER: &str = "sherpa-onnx-spleeter-2stems-int8";
const UVR_METADATA: &[&str] = &[
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

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Vocals,
    Instrumental,
}

impl Target {
    pub(super) fn vocal_index(self) -> usize {
        match self {
            Self::Vocals => 0,
            Self::Instrumental => 1,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) enum Engine {
    Spleeter {
        vocals: PathBuf,
        accompaniment: PathBuf,
    },
    Uvr(PathBuf),
}

impl Engine {
    pub(super) fn validate(&self) -> Result<()> {
        match self {
            Self::Spleeter {
                vocals,
                accompaniment,
            } => {
                inspect(vocals, "spleeter", &[])?;
                inspect(accompaniment, "spleeter", &[])
            }
            Self::Uvr(model) => inspect(model, "UVR", UVR_METADATA),
        }
    }

    pub(super) fn label(&self) -> &'static str {
        match self {
            Self::Spleeter { .. } => "Spleeter 2stems",
            Self::Uvr(_) => "UVR MDX-Net",
        }
    }
}

enum Source {
    Engine(Engine),
    Archive(PathBuf),
}

pub struct Model {
    pub(super) id: String,
    pub name: String,
    pub target: Option<Target>,
    source: Source,
}

impl Model {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub(super) fn resolve(self, scratch: &Path) -> Result<Engine> {
        match self.source {
            Source::Engine(engine) => Ok(engine),
            Source::Archive(archive) => extract_spleeter(&archive, scratch),
        }
    }
}

pub struct Catalog {
    pub models: Vec<Model>,
    pub accompaniment: PathBuf,
}

impl Catalog {
    pub fn discover(models_root: &Path) -> Result<Self> {
        let (models, accompaniment) = Self::inventory(models_root)?;
        let accompaniment = accompaniment.with_context(|| {
            format!(
                "缺少固定伴奏模型 {ACCOMPANIMENT}.onnx，可放入 {}",
                models_root.join("separation").display()
            )
        })?;
        ensure!(
            !models.is_empty(),
            "未找到人声分离模型，可将 Spleeter 或 UVR 模型放入 {}",
            models_root.join("separation").display()
        );
        Ok(Self {
            models,
            accompaniment,
        })
    }

    pub fn document(models_root: &Path) -> Result<serde_json::Value> {
        use serde_json::json;

        let (models, accompaniment) = Self::inventory(models_root)?;
        let descriptor = |model: &Model| {
            let engine = match &model.source {
                Source::Engine(engine) => engine.label(),
                Source::Archive(_) => "Spleeter 2stems",
            };
            json!({
                "id": model.id, "name": model.name, "engine": engine,
                "target": model.target.unwrap_or(Target::Vocals), "targetKnown": model.target.is_some(),
                "available": true, "description": "人声分离", "detail": "",
            })
        };
        let mut vocals = Vec::new();
        for (id, name, engine) in [
            (
                "spleeter-2stems-int8",
                "Spleeter 2stems INT8",
                "Spleeter 2stems",
            ),
            ("uvr-voc-ft", VOCALS, "UVR MDX-Net"),
        ] {
            let model = models.iter().find(|entry| entry.id == id);
            vocals.push(model.map_or_else(|| json!({
                "id":id, "name":name, "engine":engine, "target":"vocals", "targetKnown":true,
                "available":false, "description":"人声分离", "detail":"未找到模型文件",
            }), descriptor));
        }
        vocals.extend(
            models
                .iter()
                .filter(|model| model.id.starts_with("custom:"))
                .map(descriptor),
        );
        Ok(json!({
            "vocals": vocals,
            "accompaniment": {
                "id":"uvr-inst-hq-3", "name":ACCOMPANIMENT, "engine":"UVR MDX-Net",
                "target":"instrumental", "targetKnown":true, "available":accompaniment.is_some(),
                "description":"固定伴奏模型", "detail":if accompaniment.is_some() { "" } else { "未找到伴奏模型文件" },
            },
        }))
    }

    pub fn for_task(
        mut self,
        id: &str,
        target: Option<Target>,
    ) -> Result<(Model, Target, PathBuf)> {
        let index = self
            .models
            .iter()
            .position(|model| model.id == id)
            .with_context(|| format!("人声分离模型不存在：{id}"))?;
        let model = self.models.remove(index);
        let target = model
            .target
            .or(target)
            .context("请指定自定义人声分离模型的输出类型")?;
        Ok((model, target, self.accompaniment))
    }

    fn inventory(models_root: &Path) -> Result<(Vec<Model>, Option<PathBuf>)> {
        let executable = std::env::current_exe()?;
        let program = executable.parent().context("找不到应用所在目录")?;
        let bundled = program
            .parent()
            .filter(|path| path.ends_with("Contents"))
            .map(|path| path.join("Resources/models"));
        let custom = models_root.join("separation");
        let resources: Vec<_> = bundled
            .into_iter()
            .chain([program.join("models"), custom.clone()])
            .collect();
        let locate = |file: &str| {
            resources
                .iter()
                .map(|dir| dir.join(file))
                .find(|path| regular(path))
        };

        let accompaniment_name = format!("{ACCOMPANIMENT}.onnx");
        let accompaniment = locate(&accompaniment_name);
        let mut models = Vec::new();
        if let Some(source) = resources
            .iter()
            .find_map(|dir| spleeter(&dir.join(SPLEETER)).map(Source::Engine))
            .or_else(|| locate(&format!("{SPLEETER}.tar.bz2")).map(Source::Archive))
        {
            models.push(Model {
                id: "spleeter-2stems-int8".into(),
                name: "Spleeter 2stems INT8".into(),
                target: Some(Target::Vocals),
                source,
            });
        }
        let vocals_name = format!("{VOCALS}.onnx");
        if let Some(path) = locate(&vocals_name) {
            models.push(Model {
                id: "uvr-voc-ft".into(),
                name: VOCALS.into(),
                target: Some(Target::Vocals),
                source: Source::Engine(Engine::Uvr(path)),
            });
        }
        match fs::read_dir(&custom) {
            Ok(entries) => {
                let mut local = Vec::new();
                for entry in entries {
                    let entry = entry?;
                    let path = entry.path();
                    let kind = entry.file_type()?;
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let (engine, target) = if kind.is_dir() && name != SPLEETER {
                        let Some(engine) = spleeter(&path) else {
                            continue;
                        };
                        (engine, Some(Target::Vocals))
                    } else if kind.is_file()
                        && is_onnx(&path)
                        && !name.eq_ignore_ascii_case(&accompaniment_name)
                        && name != vocals_name
                    {
                        (Engine::Uvr(path), None)
                    } else {
                        continue;
                    };
                    local.push(Model {
                        id: format!("custom:{name}"),
                        name,
                        target,
                        source: Source::Engine(engine),
                    });
                }
                local.sort_by_key(|model| model.name.to_lowercase());
                models.extend(local);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("无法读取 {}", custom.display()));
            }
        }
        Ok((models, accompaniment))
    }
}

fn regular(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
}

fn is_onnx(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("onnx"))
}

fn spleeter(dir: &Path) -> Option<Engine> {
    let mut files: Vec<_> = fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_onnx(path) && regular(path))
        .collect();
    files.sort();
    let find = |prefix| {
        files
            .iter()
            .find(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().to_lowercase().starts_with(prefix))
            })
            .cloned()
    };
    Some(Engine::Spleeter {
        vocals: find("vocals")?,
        accompaniment: find("accompaniment")?,
    })
}

fn extract_spleeter(archive: &Path, scratch: &Path) -> Result<Engine> {
    let mut paths = Vec::new();
    for name in ["vocals.int8.onnx", "accompaniment.int8.onnx"] {
        let dest = scratch.join(name);
        let mut found = false;
        for entry in [
            format!("{SPLEETER}/{name}"),
            format!("./{SPLEETER}/{name}"),
            name.into(),
            format!("./{name}"),
        ] {
            let output = File::create(&dest)?;
            let status = Command::new("tar")
                .args(["-xOjf"])
                .arg(archive)
                .arg(entry)
                .stdout(output)
                .stderr(Stdio::null())
                .status()
                .context("无法启动 tar")?;
            if status.success() && regular(&dest) {
                found = true;
                break;
            }
        }
        ensure!(found, "Spleeter 压缩包缺少 {name}：{}", archive.display());
        paths.push(dest);
    }
    let [vocals, accompaniment] = paths
        .try_into()
        .map_err(|_| anyhow::anyhow!("Spleeter 压缩包缺少双轨模型"))?;
    Ok(Engine::Spleeter {
        vocals,
        accompaniment,
    })
}

fn inspect(path: &Path, expected: &str, required: &[&str]) -> Result<()> {
    let metadata =
        read_metadata(path).with_context(|| format!("无法读取 ONNX 元数据：{}", path.display()))?;
    ensure!(
        metadata
            .get("model_type")
            .is_some_and(|kind| kind == expected)
            && metadata.get("stems").is_some_and(|count| count == "2"),
        "{} 不是 sherpa-onnx 兼容的双轨 {expected} 模型",
        path.display()
    );
    for key in required {
        ensure!(
            metadata.get(*key).is_some_and(|value| !value.is_empty()),
            "{} 缺少 ONNX 元数据 {key}",
            path.display()
        );
    }
    Ok(())
}

fn read_metadata(path: &Path) -> Result<HashMap<String, String>> {
    let file = File::open(path)?;
    let length = file.metadata()?.len();
    let mut input = BufReader::new(file);
    let mut fields = HashMap::new();
    while input.stream_position()? < length {
        let tag = varint(&mut input)?;
        let field_length = match tag & 7 {
            0 => {
                varint(&mut input)?;
                continue;
            }
            1 => 8,
            2 => varint(&mut input)?,
            5 => 4,
            _ => bail!("无效的 ONNX 字段类型"),
        };
        let end = input
            .stream_position()?
            .checked_add(field_length)
            .context("ONNX 字段长度溢出")?;
        ensure!(end <= length, "ONNX 字段超出文件");
        if tag == (14 << 3 | 2) && field_length <= 8192 {
            let mut bytes = vec![0; field_length as usize];
            input.read_exact(&mut bytes)?;
            let mut entry = bytes.as_slice();
            let mut key = None;
            let mut value = None;
            while !entry.is_empty() {
                let tag = varint(&mut entry)?;
                ensure!(tag & 7 == 2, "无效的 ONNX 元数据字段");
                let length = usize::try_from(varint(&mut entry)?)?;
                let (data, rest) = entry
                    .split_at_checked(length)
                    .context("ONNX 元数据字段超出记录")?;
                match tag >> 3 {
                    1 => key = Some(std::str::from_utf8(data)?.to_owned()),
                    2 => value = Some(std::str::from_utf8(data)?.to_owned()),
                    _ => {}
                }
                entry = rest;
            }
            if let (Some(key), Some(value)) = (key, value)
                && (matches!(key.as_str(), "model_type" | "stems")
                    || UVR_METADATA.contains(&key.as_str()))
            {
                fields.insert(key, value);
            }
        } else {
            input.seek(SeekFrom::Start(end))?;
        }
    }
    Ok(fields)
}

fn varint(input: &mut impl Read) -> Result<u64> {
    let mut result = 0;
    for shift in (0..64).step_by(7) {
        let mut byte = [0];
        input.read_exact(&mut byte)?;
        ensure!(shift < 63 || byte[0] <= 1, "ONNX 整数溢出");
        result |= u64::from(byte[0] & 0x7f) << shift;
        if byte[0] & 0x80 == 0 {
            return Ok(result);
        }
    }
    bail!("无效的 ONNX 整数")
}
