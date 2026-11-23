//! 字幕与人声分离记录的 HTTP 参数和后台操作。

use std::collections::HashMap;
use std::path::Path;

use http::{Method, StatusCode, Uri};
use serde_json::{Value, json};

use crate::library::{
    SubtitleEdit, SubtitleFormat, SubtitleSegment, SubtitleWriteError, delete_subtitle_if_present,
    subtitle_paths,
};

use super::{Failure, Request};

pub enum Operation {
    List {
        offset: usize,
        limit: usize,
        search: Option<String>,
        include_content: bool,
    },
    Track(String),
    Segments(String, Vec<SubtitleSegment>),
    Text(String, SubtitleFormat, String),
    Delete(String),
    Export(String, SubtitleFormat),
    SeparationResult(String),
}

pub enum Payload {
    Json(Value),
    Download { filename: String, content: String },
}

pub type Reply = Result<Payload, Failure>;

impl Operation {
    pub(super) fn is_route(method: &Method, uri: &Uri) -> bool {
        match uri.path() {
            "/v1/subtitles" | "/v1/subtitles/export" | "/v1/separation/result" => {
                *method == Method::GET
            }
            "/v1/subtitles/track" => matches!(*method, Method::GET | Method::PUT | Method::DELETE),
            "/v1/subtitles/text" => *method == Method::PUT,
            _ => false,
        }
    }

    pub(super) fn parse(request: &Request) -> Result<Self, Failure> {
        let query: HashMap<_, _> =
            url::form_urlencoded::parse(request.uri.query().unwrap_or_default().as_bytes())
                .into_owned()
                .collect();
        let parameter = |name: &str| {
            query
                .get(name)
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
        };
        let path = request.uri.path();
        if path == "/v1/subtitles" {
            let number = |name: &str, default| {
                parameter(name).map_or(Ok(default), |raw| {
                    raw.parse::<usize>()
                        .map_err(|_| bad(format!("{name} 必须是非负整数")))
                })
            };
            return Ok(Self::List {
                offset: number("offset", 0)?,
                limit: number("limit", 200)?.clamp(1, 1000),
                search: parameter("q").map(str::to_lowercase),
                include_content: parameter("include").is_some_and(|value| {
                    value
                        .split(',')
                        .any(|field| field.trim().eq_ignore_ascii_case("content"))
                }),
            });
        }
        let body = if request.method == Method::PUT {
            serde_json::from_slice::<Value>(&request.body)
                .ok()
                .filter(Value::is_object)
                .ok_or_else(|| bad("请求体需要 JSON 对象"))?
        } else {
            Value::Null
        };
        let key = parameter("key")
            .or_else(|| {
                body["key"]
                    .as_str()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
            })
            .ok_or_else(|| bad("缺少曲目 key"))?
            .to_owned();
        let format = || {
            let raw = query
                .get("format")
                .map(String::as_str)
                .or_else(|| body["format"].as_str())
                .unwrap_or("srt")
                .trim();
            if raw.eq_ignore_ascii_case("srt") {
                Ok(SubtitleFormat::Srt)
            } else if raw.eq_ignore_ascii_case("lrc") {
                Ok(SubtitleFormat::Lrc)
            } else {
                Err(bad("字幕格式须为 srt 或 lrc"))
            }
        };
        match (&request.method, path) {
            (&Method::GET, "/v1/subtitles/track") => Ok(Self::Track(key)),
            (&Method::PUT, "/v1/subtitles/track") => {
                let segments = body.get("segments").ok_or_else(|| bad("缺少 segments"))?;
                let segments = serde_json::from_value::<Vec<SubtitleSegment>>(segments.clone())
                    .map_err(|error| bad(format!("segments 格式错误：{error}")))?;
                Ok(Self::Segments(key, segments))
            }
            (&Method::PUT, "/v1/subtitles/text") => {
                let text = body["text"].as_str().ok_or_else(|| bad("缺少 text"))?;
                Ok(Self::Text(key, format()?, text.to_owned()))
            }
            (&Method::DELETE, "/v1/subtitles/track") => Ok(Self::Delete(key)),
            (&Method::GET, "/v1/subtitles/export") => Ok(Self::Export(key, format()?)),
            (&Method::GET, "/v1/separation/result") => Ok(Self::SeparationResult(key)),
            _ => Err((StatusCode::NOT_FOUND, "接口不存在".into())),
        }
    }

    pub fn write_key(&self) -> Option<&str> {
        match self {
            Self::Segments(key, _) | Self::Text(key, _, _) | Self::Delete(key) => Some(key),
            _ => None,
        }
    }

    pub fn track_key(&self) -> Option<&str> {
        match self {
            Self::Track(key) | Self::Export(key, _) => Some(key),
            _ => self.write_key(),
        }
    }

    pub fn execute(self, directory: &Path, song: Option<&bgm_core::Song>) -> Reply {
        let read = |key: &str| {
            SubtitleEdit::find(directory, key)
                .map_err(storage)?
                .ok_or_else(|| (StatusCode::NOT_FOUND, format!("字幕记录不存在：{key}")))
        };
        let value = match self {
            Self::List {
                offset,
                limit,
                search,
                include_content,
            } => list(directory, offset, limit, search.as_deref(), include_content)?,
            Self::Track(key) => read(&key)?.document(true),
            Self::Segments(key, segments) => read(&key)?
                .save_segments_record(segments)
                .map_err(write_failure)?,
            Self::Text(key, format, raw) => read(&key)?
                .save_text_record(format, &raw, song.and_then(|song| song.duration_ms))
                .map_err(write_failure)?,
            Self::Delete(key) => {
                let deleted = delete_subtitle_if_present(directory, &key).map_err(storage)?;
                json!({"key": key, "deleted": deleted})
            }
            Self::SeparationResult(key) => {
                crate::library::separation::result_document(directory, &key)
                    .map_err(storage)?
                    .ok_or_else(|| {
                        (
                            StatusCode::NOT_FOUND,
                            format!("该曲目尚未执行人声分离：{key}"),
                        )
                    })?
            }
            Self::Export(key, format) => {
                let record = read(&key)?;
                let metadata = record.document(false);
                let title = song
                    .map(|song| song.title.as_str())
                    .or_else(|| metadata["title"].as_str())
                    .unwrap_or(&key);
                let stem = bgm_core::export::safe_file_name(title);
                return Ok(Payload::Download {
                    filename: format!("{stem}.{}", format.extension()),
                    content: record.text(format),
                });
            }
        };
        Ok(Payload::Json(value))
    }
}

fn list(
    directory: &Path,
    offset: usize,
    limit: usize,
    search: Option<&str>,
    include_content: bool,
) -> Result<Value, Failure> {
    let mut total = 0;
    let mut page = Vec::new();
    for (summary, path) in subtitle_paths(directory).map_err(storage)? {
        let Ok(record) = SubtitleEdit::from_path(path, &summary.key) else {
            continue;
        };
        let metadata = record.document(false);
        if let Some(needle) = search
            && !["title", "key", "fileName"].iter().any(|field| {
                metadata[*field]
                    .as_str()
                    .is_some_and(|value| value.to_lowercase().contains(needle))
            })
            && ![SubtitleFormat::Srt, SubtitleFormat::Lrc]
                .into_iter()
                .any(|format| record.text(format).to_lowercase().contains(needle))
        {
            continue;
        }
        if total >= offset && page.len() < limit {
            let mut value = if include_content {
                record.document(true)
            } else {
                metadata
            };
            value["segmentCount"] = json!(record.segments().len());
            page.push(value);
        }
        total += 1;
    }
    Ok(json!({
        "records": page,
        "total": total,
        "offset": offset,
        "limit": limit,
        "includeContent": include_content,
    }))
}

fn bad(message: impl Into<String>) -> Failure {
    (StatusCode::BAD_REQUEST, message.into())
}

fn storage(message: String) -> Failure {
    (StatusCode::INTERNAL_SERVER_ERROR, message)
}

fn write_failure(error: SubtitleWriteError) -> Failure {
    match error {
        SubtitleWriteError::Invalid(message) => bad(message),
        SubtitleWriteError::Conflict(message) => (StatusCode::CONFLICT, message),
        SubtitleWriteError::Storage(message) => storage(message),
    }
}

pub(super) fn disposition(filename: &str) -> String {
    use std::fmt::Write;
    let mut ascii = String::new();
    let mut utf8 = String::new();
    for ch in filename.chars() {
        ascii.push(
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ' ') {
                ch
            } else {
                '_'
            },
        );
    }
    for byte in filename.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            utf8.push(char::from(byte));
        } else {
            let _ = write!(utf8, "%{byte:02X}");
        }
    }
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{utf8}")
}
