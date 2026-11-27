//! ASR 模型目录偏好与本地 ONNX 模型发现。

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use bgm_core::Prefs;
use url::Url;

mod download;
mod worker;
use download::DefaultModelFiles;
pub use download::{DEFAULT_MODEL_NAME, DownloadMode, DownloadProgress, ModelDownload};
pub use worker::{AsrSession, WORKER_FLAG, run_asr_worker};

pub use bgm_core::prefs::DEFAULT_ASR_DOWNLOAD_URL;
const MAX_SCAN_DEPTH: usize = 4;
const MODEL_FILES: [&str; 3] = [
    "conv_frontend.onnx",
    "encoder.int8.onnx",
    "decoder.int8.onnx",
];

pub struct ModelPreferences {
    pub directory: Option<PathBuf>,
    pub download_url: String,
    pub last_model: Option<String>,
}

impl ModelPreferences {
    pub(super) fn from_prefs(prefs: &Prefs) -> Self {
        Self {
            directory: prefs
                .models_dir
                .as_deref()
                .and_then(model_directory)
                .map(PathBuf::from),
            download_url: if valid_download_url(&prefs.asr_download_url) {
                prefs.asr_download_url.clone()
            } else {
                DEFAULT_ASR_DOWNLOAD_URL.to_owned()
            },
            last_model: prefs
                .last_asr_model
                .as_deref()
                .and_then(model_id)
                .map(str::to_owned),
        }
    }

    pub fn root(&self) -> PathBuf {
        self.directory.clone().unwrap_or_else(|| {
            dirs::download_dir()
                .or_else(|| dirs::home_dir().map(|home| home.join("Downloads")))
                .unwrap_or_else(|| PathBuf::from("Downloads"))
                .join("bgm-models")
        })
    }
}

pub(crate) fn model_id(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()
        && value.chars().count() <= 512
        && !value.contains(['\\', ':', '\0'])
        && value
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != ".."))
    .then_some(value)
}

pub(super) fn model_directory(value: &str) -> Option<&str> {
    let value = value.trim();
    let value = value.strip_prefix(r"\\?\").unwrap_or(value);
    (!value.is_empty() && !value.contains('\0') && value.chars().count() <= 512).then_some(value)
}

pub(super) fn valid_download_url(value: &str) -> bool {
    value.chars().count() <= 1024
        && !value.contains(char::is_whitespace)
        && Url::parse(value)
            .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.has_host())
}

pub fn default_model_complete(root: &Path) -> bool {
    DefaultModelFiles::inspect(root).missing.is_empty()
}

pub struct ModelEntry {
    pub id: String,
    pub files: Vec<String>,
    pub missing_components: Option<Vec<&'static str>>,
}

pub struct ModelCatalog {
    pub root: PathBuf,
    pub is_default_root: bool,
    pub models: Vec<ModelEntry>,
    pub warnings: Vec<String>,
    pub default_files: DefaultModelFiles,
}

impl ModelCatalog {
    pub fn scan(preferences: &ModelPreferences) -> Self {
        let root = preferences.root();
        let mut catalog = Self {
            default_files: DefaultModelFiles::inspect(&root),
            root,
            is_default_root: preferences.directory.is_none(),
            models: Vec::new(),
            warnings: Vec::new(),
        };
        catalog.scan_directory(Path::new(""), 0);
        catalog.models.sort_by(|left, right| left.id.cmp(&right.id));
        catalog.warnings.sort();
        catalog
    }

    fn scan_directory(&mut self, relative: &Path, depth: usize) {
        let directory = self.root.join(relative);
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                if !(depth == 0 && self.is_default_root && error.kind() == ErrorKind::NotFound) {
                    self.warn(&directory, &error);
                }
                return;
            }
        };
        let mut files = Vec::new();
        let mut has_tokenizer = false;
        for entry in entries {
            let (entry, kind) = match entry.and_then(|entry| {
                let kind = entry.file_type()?;
                Ok((entry, kind))
            }) {
                Ok(entry) => entry,
                Err(error) => {
                    self.warn(&directory, &error);
                    continue;
                }
            };
            let name = entry.file_name();
            let label = name.to_string_lossy();
            if kind.is_dir() {
                has_tokenizer |= label == "tokenizer";
                if depth < MAX_SCAN_DEPTH
                    && !label.starts_with('.')
                    && !label.eq_ignore_ascii_case("separation")
                    && !(depth == 0 && label.eq_ignore_ascii_case("tts"))
                {
                    self.scan_directory(&relative.join(&name), depth + 1);
                }
            } else if kind.is_file()
                && Path::new(&name)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("onnx"))
            {
                files.push(label.into_owned());
            }
        }

        if depth == 0 {
            self.models.extend(files.into_iter().map(|file| {
                let name = Path::new(&file)
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or(&file);
                ModelEntry {
                    id: format!("file:{name}"),
                    files: vec![file],
                    missing_components: None,
                }
            }));
        } else if !files.is_empty() {
            files.sort();
            let mut missing: Vec<_> = MODEL_FILES
                .into_iter()
                .filter(|name| !files.iter().any(|file| file == name))
                .collect();
            if !has_tokenizer {
                missing.push("tokenizer/");
            }
            self.models.push(ModelEntry {
                id: relative
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/"),
                files,
                missing_components: Some(missing),
            });
        }
    }

    fn warn(&mut self, path: &Path, error: &std::io::Error) {
        self.warnings
            .push(format!("无法扫描模型目录 {}：{error}", path.display()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_groups_components_and_filters_hidden_and_deep_directories() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let complete = root.join("first/second/third/model");
        fs::create_dir_all(complete.join("tokenizer")).unwrap();
        for file in MODEL_FILES {
            fs::write(complete.join(file), b"fixture").unwrap();
        }
        for directory in [
            ".hidden/model",
            "separation/model",
            "tts/kokoro-int8-multi-lang-v1_1",
            "first/second/third/model/fifth",
            "incomplete",
        ] {
            let path = root.join(directory);
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join("encoder.int8.onnx"), b"fixture").unwrap();
        }
        fs::write(root.join("loose.onnx"), b"fixture").unwrap();
        fs::write(root.join("notes.txt"), b"notes").unwrap();

        let settings = ModelPreferences {
            last_model: None,
            directory: Some(root.to_owned()),
            download_url: DEFAULT_ASR_DOWNLOAD_URL.into(),
        };
        let catalog = ModelCatalog::scan(&settings);
        assert!(catalog.warnings.is_empty());
        assert!(!catalog.is_default_root);
        assert_eq!(
            catalog
                .models
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            ["file:loose", "first/second/third/model", "incomplete"]
        );
        assert_eq!(catalog.models[0].missing_components, None);
        assert_eq!(catalog.models[1].missing_components, Some(Vec::new()));
        assert_eq!(catalog.models[1].files.len(), 3);
        assert_eq!(
            catalog.models[2].missing_components.as_deref(),
            Some(["conv_frontend.onnx", "decoder.int8.onnx", "tokenizer/"].as_slice())
        );
    }

    #[cfg(unix)]
    #[test]
    fn scan_ignores_symlink_models_and_components() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("models");
        let external = temp.path().join("external");
        let incomplete = root.join("incomplete");
        fs::create_dir_all(&incomplete).unwrap();
        fs::create_dir_all(external.join("tokenizer")).unwrap();
        fs::write(external.join("encoder.int8.onnx"), b"external").unwrap();
        fs::write(incomplete.join("conv_frontend.onnx"), b"fixture").unwrap();
        symlink(&external, root.join("linked-model")).unwrap();
        symlink(external.join("tokenizer"), incomplete.join("tokenizer")).unwrap();
        symlink(
            external.join("encoder.int8.onnx"),
            incomplete.join("encoder.int8.onnx"),
        )
        .unwrap();
        let preferences = ModelPreferences {
            last_model: None,
            directory: Some(root),
            download_url: DEFAULT_ASR_DOWNLOAD_URL.into(),
        };

        let catalog = ModelCatalog::scan(&preferences);
        assert_eq!(catalog.models.len(), 1);
        assert_eq!(catalog.models[0].files, ["conv_frontend.onnx"]);
        assert_eq!(
            catalog.models[0].missing_components.as_deref(),
            Some(["encoder.int8.onnx", "decoder.int8.onnx", "tokenizer/"].as_slice())
        );
    }

    #[test]
    fn invalid_saved_model_preferences_use_defaults() {
        let prefs = Prefs {
            models_dir: Some("a".repeat(513)),
            asr_download_url: "file:///tmp/model.onnx".into(),
            ..Default::default()
        };
        let preferences = ModelPreferences::from_prefs(&prefs);
        assert!(preferences.directory.is_none());
        assert!(preferences.root().ends_with("bgm-models"));
        assert_eq!(preferences.download_url, DEFAULT_ASR_DOWNLOAD_URL);
        assert_eq!(model_directory(r"  \\?\C:\models  "), Some(r"C:\models"));
        assert!(!valid_download_url("https://example.com/has spaces"));
        assert!(valid_download_url("https://example.com/has%20spaces"));
    }
}
