//! 从公开分享地址读取歌单曲序和艺人，行为对齐主线 `bgm-tauri2`。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::net::{IpAddr, ToSocketAddrs};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;
use url::Url;

pub const MAX_INPUTS: usize = 20;
const PAGE_SIZE: usize = 100;
const MAX_PAGES: usize = 30;
const MAX_TRACKS: usize = PAGE_SIZE * MAX_PAGES;
const MAX_REDIRECTS: usize = 4;
const MAX_URL: usize = 4096;
const MAX_BODY: u64 = 8 * 1024 * 1024;
const REQUEST_TIME: Duration = Duration::from_secs(10);
const SOURCE_TIME: Duration = Duration::from_secs(45);

const INVALID_URL: &str = "请提供有效的 HTTPS 歌单链接";
const UNSUPPORTED: &str = "不支持此链接，请复制网易云音乐、QQ 音乐或汽水音乐的歌单分享链接";
const REDIRECT: &str = "歌单短链接已失效，或跳转目标不是同一平台的歌单";
const UNAVAILABLE: &str = "歌单不存在、已失效或不是公开歌单，请检查分享链接";
const FORMAT: &str = "平台没有返回可读取的公开歌单数据，可能需要登录或链接已失效";
const INCOMPLETE: &str = "平台未返回全部曲目";
const CHANGED: &str = "歌单分页重复或顺序已变化，请重试";
const LIMIT: &str = "已达到单个歌单的读取上限";
const NETWORK: &str = "无法连接音乐平台，请检查网络后重试";
const TIMEOUT: &str = "读取歌单超时，请稍后重试";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Platform {
    Netease,
    Qq,
    Qishui,
}

impl Platform {
    fn label(self) -> &'static str {
        match self {
            Self::Netease => "网易云音乐",
            Self::Qq => "QQ 音乐",
            Self::Qishui => "汽水音乐",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Source {
    Playlist(Platform, u64),
    Short(Platform, Url),
}

pub struct Playlist {
    pub platform: &'static str,
    pub title: String,
    pub tracks: Vec<Track>,
    pub expected_total: Option<usize>,
    pub warning: Option<String>,
}

impl Playlist {
    fn new(platform: Platform, title: String, expected_total: Option<usize>) -> Self {
        Self {
            platform: platform.label(),
            title,
            tracks: Vec::new(),
            expected_total,
            warning: None,
        }
    }

    fn finish(mut self, problem: Option<&'static str>) -> Result<Self, String> {
        let actual = self.tracks.len();
        let problem = problem.or_else(|| match self.expected_total {
            Some(total) if total == actual => None,
            Some(_) => Some(INCOMPLETE),
            None => Some("平台未提供曲目总数，无法确认歌单是否完整"),
        });
        if let Some(reason) = problem {
            if actual == 0 {
                return Err(reason.to_owned());
            }
            self.warning = Some(match self.expected_total {
                Some(total) => format!("{reason}，已读取 {actual} / {total} 首"),
                None => format!("{reason}，已读取 {actual} 首"),
            });
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub title: String,
    pub artists: Vec<String>,
}

impl Track {
    pub fn line(&self) -> String {
        if self.artists.is_empty() {
            self.title.clone()
        } else {
            format!("{} - {}", self.title, self.artists.join("、"))
        }
    }
}

pub struct Outcome {
    pub input: String,
    pub result: Result<Playlist, String>,
}

pub fn share_inputs(text: &str) -> Vec<String> {
    let mut inputs = Vec::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let mut found = false;
        let mut urls: Vec<_> = line
            .match_indices("https://")
            .chain(line.match_indices("http://"))
            .collect();
        urls.sort_unstable_by_key(|(start, _)| *start);
        for (start, _) in urls {
            found = true;
            let link = line[start..]
                .split(|ch: char| {
                    ch.is_whitespace()
                        || ch.is_control()
                        || "，。；、！？【】《》“”‘’（）「」<>\"".contains(ch)
                })
                .next()
                .unwrap_or_default()
                .trim_end_matches([',', ';', ')', ']']);
            let normalized = if let Some(rest) = link.strip_prefix("http://") {
                format!("https://{rest}")
            } else {
                link.to_owned()
            };
            inputs.push(normalized.chars().take(MAX_URL).collect());
        }
        if !found {
            inputs.push(line.chars().take(MAX_URL).collect());
        }
        if inputs.len() > MAX_INPUTS {
            break;
        }
    }
    inputs.truncate(MAX_INPUTS + 1);
    inputs
}

pub fn resolve(input: &str, cancelled: &AtomicBool) -> Result<Playlist, String> {
    let source = recognize(input)?;
    let http = Http {
        cancelled,
        deadline: Instant::now() + SOURCE_TIME,
    };
    let (platform, id) = http.resolve(source)?;
    match platform {
        Platform::Netease => netease(&http, id),
        Platform::Qq => qq_pages(id, |offset| {
            http.json(qq_url(id, offset), Some("https://y.qq.com/"))
        }),
        Platform::Qishui => {
            let url = url_with_query(
                "https://music.douyin.com/qishui/share/playlist",
                &[("playlist_id", &id.to_string())],
            );
            let html = http.body(&url, None)?;
            qishui(&html, id)
        }
    }
}

pub fn export_text(results: &[Outcome]) -> String {
    results
        .iter()
        .filter_map(|outcome| outcome.result.as_ref().ok())
        .flat_map(|playlist| playlist.tracks.iter().map(Track::line))
        .fold(String::new(), |mut text, line| {
            text.push_str(&line);
            text.push('\n');
            text
        })
}

fn url_with_query(base: &str, query: &[(&str, &str)]) -> Url {
    let mut url = Url::parse(base).expect("static playlist endpoint");
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query.iter().copied());
    }
    url
}

fn valid_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.domain().is_some()
}

fn query_param(url: &Url, key: &str) -> Option<String> {
    let mut values = url.query_pairs().filter(|(name, _)| name == key);
    let (_, value) = values.next()?;
    values.next().is_none().then(|| value.into_owned())
}

fn playlist_id(value: &str) -> Option<u64> {
    if value.is_empty() || value.len() > 20 || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse().ok().filter(|&id| id != 0)
}

fn id_query(url: &Url, key: &str) -> Option<u64> {
    playlist_id(&query_param(url, key)?)
}

fn token(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

fn netease_id(url: &Url) -> Option<u64> {
    let parse = |url: &Url| match url.path() {
        "/playlist" | "/m/playlist" => id_query(url, "id"),
        path => playlist_id(path.strip_prefix("/playlist/")?),
    };
    if url.path() == "/"
        && let Some(fragment) = url.fragment().filter(|f| f.starts_with("/playlist"))
    {
        return parse(&url.join(fragment).ok()?);
    }
    parse(url)
}

fn qq_id(url: &Url) -> Option<u64> {
    match (url.domain()?, url.path()) {
        ("i.y.qq.com" | "y.qq.com", "/n2/m/share/details/taoge.html")
        | ("i2.y.qq.com", "/n3/other/pages/details/playlist.html") => id_query(url, "id"),
        ("y.qq.com", path) => {
            let path = path
                .strip_prefix("/n/ryqq/playlist/")
                .or_else(|| path.strip_prefix("/n/yqq/playlist/"))?;
            playlist_id(path.strip_suffix(".html").unwrap_or(path))
        }
        _ => None,
    }
}

fn recognize(raw: &str) -> Result<Source, String> {
    let url = checked_url(raw.trim())?;
    classify(&url).map_err(str::to_owned)
}

fn checked_url(raw: &str) -> Result<Url, String> {
    if raw.len() > MAX_URL
        || !raw.starts_with("https://")
        || raw.contains('\\')
        || raw.chars().any(char::is_control)
        || raw["https://".len()..]
            .split(['/', '?', '#'])
            .next()
            .is_none_or(|authority| authority.contains('@'))
    {
        return Err(INVALID_URL.to_owned());
    }
    let url = Url::parse(raw).map_err(|_| INVALID_URL.to_owned())?;
    valid_url(&url)
        .then_some(url)
        .ok_or_else(|| INVALID_URL.to_owned())
}

fn classify(url: &Url) -> Result<Source, &'static str> {
    use Platform::*;
    use Source::*;
    let host = url.domain().ok_or(INVALID_URL)?;
    let path = url.path();
    let (platform, id) = match host {
        "music.163.com" | "y.music.163.com" => (Netease, netease_id(url)),
        "y.qq.com" | "i.y.qq.com" | "i2.y.qq.com" => (Qq, qq_id(url)),
        "music.douyin.com" if path == "/qishui/share/playlist" => {
            (Qishui, id_query(url, "playlist_id"))
        }
        "www.qishui.com" if path == "/share/playlist" => (Qishui, id_query(url, "playlist_id")),
        "www.qishui.com" | "qishui.douyin.com" if path.starts_with("/playlist/") => {
            (Qishui, playlist_id(&path["/playlist/".len()..]))
        }
        "163cn.tv" => {
            let code = path.trim_start_matches('/');
            if token(code) && path.len() == code.len() + 1 {
                return Ok(Short(
                    Netease,
                    url_with_query(&format!("https://163cn.tv/{code}"), &[]),
                ));
            }
            return Err(UNSUPPORTED);
        }
        "c6.y.qq.com" if path == "/base/fcgi-bin/u" => {
            let code = query_param(url, "__")
                .filter(|s| token(s))
                .ok_or(UNSUPPORTED)?;
            return Ok(Short(
                Qq,
                url_with_query("https://c6.y.qq.com/base/fcgi-bin/u", &[("__", &code)]),
            ));
        }
        "qishui.douyin.com" if path.starts_with("/s/") => {
            let code = path.strip_prefix("/s/").expect("matched prefix");
            let code = code.strip_suffix('/').unwrap_or(code);
            if token(code) {
                return Ok(Short(
                    Qishui,
                    url_with_query(&format!("https://qishui.douyin.com/s/{code}/"), &[]),
                ));
            }
            return Err(UNSUPPORTED);
        }
        _ => return Err(UNSUPPORTED),
    };
    id.map(|id| Playlist(platform, id)).ok_or(UNSUPPORTED)
}

fn redirect_source(base: &Url, location: &str, platform: Platform) -> Result<Source, &'static str> {
    if location.len() > MAX_URL
        || location.contains(['\\', '@'])
        || location.chars().any(char::is_control)
    {
        return Err(REDIRECT);
    }
    let url = base.join(location).map_err(|_| REDIRECT)?;
    let source = valid_url(&url)
        .then(|| classify(&url))
        .transpose()
        .map_err(|_| REDIRECT)?
        .ok_or(REDIRECT)?;
    let (Source::Playlist(target, _) | Source::Short(target, _)) = &source;
    (*target == platform).then_some(source).ok_or(REDIRECT)
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !matches!(
                (a, b, c),
                (0 | 10 | 127 | 224..=255, _, _)
                    | (100, 64..=127, _)
                    | (169, 254, _)
                    | (172, 16..=31, _)
                    | (192, 0, 0 | 2)
                    | (192, 88, 99)
                    | (192, 168, _)
                    | (198, 18 | 19, _)
                    | (198, 51, 100)
                    | (203, 0, 113)
            )
        }
        IpAddr::V6(ip) => {
            let [a, b, ..] = ip.segments();
            (0x2000..=0x3fff).contains(&a)
                && !matches!((a, b), (0x2001, 0..=0x1ff | 0x0db8) | (0x2002 | 0x3fff, _))
        }
    }
}

fn is_fake_ip(ip: IpAddr) -> bool {
    matches!(ip, IpAddr::V4(ip) if matches!(ip.octets(), [198, 18 | 19, _, _]))
}

fn parse_doh_public_ip(body: &[u8]) -> Option<IpAddr> {
    let parsed: Value = serde_json::from_slice(body).ok()?;
    parsed["Answer"]
        .as_array()?
        .iter()
        .filter(|entry| entry["type"].as_u64() == Some(1))
        .filter_map(|entry| entry["data"].as_str()?.parse::<IpAddr>().ok())
        .find(|ip| public_ip(*ip))
}

fn resolve_doh_public_ip(host: &str) -> Option<IpAddr> {
    let url = url_with_query(
        "https://223.5.5.5/resolve",
        &[("name", host), ("type", "A")],
    );
    let output = Command::new("/usr/bin/curl")
        .args([
            "--disable",
            "--silent",
            "--show-error",
            "--noproxy",
            "*",
            "--proto",
            "=https",
            "--connect-timeout",
            "3",
            "--max-time",
            "5",
            "--max-filesize",
            "65536",
            "--",
            url.as_str(),
        ])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| parse_doh_public_ip(&output.stdout))
        .flatten()
}

fn resolve_public_ip(host: &str) -> Result<IpAddr, String> {
    let ips: Vec<IpAddr> = (host, 443)
        .to_socket_addrs()
        .map_err(|_| "无法解析音乐平台域名".to_owned())?
        .map(|addr| addr.ip())
        .collect();
    if let Some(ip) = ips.iter().copied().find(|ip| public_ip(*ip)) {
        return Ok(ip);
    }
    if !ips.is_empty()
        && ips.iter().copied().all(is_fake_ip)
        && let Some(ip) = resolve_doh_public_ip(host)
    {
        return Ok(ip);
    }
    Err("音乐平台解析到了无效的网络地址".to_owned())
}

struct Http<'a> {
    cancelled: &'a AtomicBool,
    deadline: Instant,
}

struct Response {
    body: Vec<u8>,
    status: u16,
    redirect: Option<String>,
}

impl Http<'_> {
    fn resolve(&self, mut source: Source) -> Result<(Platform, u64), String> {
        let mut seen = HashSet::new();
        for _ in 0..=MAX_REDIRECTS {
            match source {
                Source::Playlist(platform, id) => return Ok((platform, id)),
                Source::Short(platform, url) => {
                    if seen.len() >= MAX_REDIRECTS || !seen.insert(url.clone()) {
                        return Err(REDIRECT.to_owned());
                    }
                    let response = self.get(&url, None)?;
                    let location = response
                        .redirect
                        .filter(|_| (300..400).contains(&response.status))
                        .ok_or_else(|| REDIRECT.to_owned())?;
                    source = redirect_source(&url, &location, platform).map_err(str::to_owned)?;
                }
            }
        }
        Err(REDIRECT.to_owned())
    }

    fn get(&self, url: &Url, referer: Option<&str>) -> Result<Response, String> {
        if !valid_url(url) {
            return Err(INVALID_URL.to_owned());
        }
        let host = url.domain().ok_or_else(|| INVALID_URL.to_owned())?;
        let ip = resolve_public_ip(host)?;
        let timeout = self
            .deadline
            .saturating_duration_since(Instant::now())
            .min(REQUEST_TIME);
        if timeout.is_zero() {
            return Err(TIMEOUT.to_owned());
        }
        let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
        let body_path = directory.path().join("response");
        let output_path = directory.path().join("status");
        let file = fs::File::create(&output_path).map_err(|error| error.to_string())?;
        let timeout_secs = timeout.as_secs().max(1).to_string();
        let resolve_arg = format!("{host}:443:{ip}");
        let mut request = Command::new("/usr/bin/curl");
        request
            .args([
                "--disable",
                "--silent",
                "--show-error",
                "--noproxy",
                "*",
                "--proto",
                "=https",
                "--connect-timeout",
                "5",
                "--max-time",
                &timeout_secs,
                "--max-filesize",
                "8388608",
                "--compressed",
                "--user-agent",
                "TFY-BGM Public Playlist Migration",
                "--resolve",
                &resolve_arg,
                "--write-out",
                "%{http_code}\n%{redirect_url}",
            ])
            .arg("--output")
            .arg(&body_path);
        if let Some(referer) = referer {
            request.args(["--referer", referer]);
        }
        if self.cancelled.load(Ordering::Relaxed) {
            return Err("已取消读取".into());
        }
        let mut child = request
            .arg("--")
            .arg(url.as_str())
            .stdin(Stdio::null())
            .stdout(file)
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("无法请求歌单：{error}"))?;
        loop {
            if self.cancelled.load(Ordering::Relaxed) || Instant::now() >= self.deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(if self.cancelled.load(Ordering::Relaxed) {
                    "已取消读取"
                } else {
                    TIMEOUT
                }
                .into());
            }
            match child.try_wait() {
                Ok(Some(status)) if status.success() => break,
                Ok(Some(status)) => {
                    let reason = match status.code() {
                        Some(6) => "无法解析音乐平台域名",
                        Some(7) => NETWORK,
                        Some(28) => TIMEOUT,
                        Some(63) => "平台返回的歌单数据过大，无法读取",
                        _ => NETWORK,
                    };
                    return Err(reason.into());
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(35)),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error.to_string());
                }
            }
        }
        let metadata = fs::read_to_string(output_path).map_err(|error| error.to_string())?;
        let (status, location) = metadata.split_once('\n').ok_or("平台未返回 HTTP 状态")?;
        let status: u16 = status.parse().map_err(|_| "平台 HTTP 状态无效")?;
        let redirect = (!location.trim().is_empty()).then(|| location.trim().to_owned());
        let mut body = Vec::new();
        if let Ok(file) = fs::File::open(body_path) {
            file.take(MAX_BODY + 1)
                .read_to_end(&mut body)
                .map_err(|error| error.to_string())?;
            if body.len() as u64 > MAX_BODY {
                return Err("平台返回的歌单数据过大，无法读取".into());
            }
        }
        Ok(Response {
            body,
            status,
            redirect,
        })
    }

    fn body(&self, url: &Url, referer: Option<&str>) -> Result<Vec<u8>, String> {
        let response = self.get(url, referer)?;
        if !(200..300).contains(&response.status) {
            return Err(match response.status {
                401 | 403 | 404 | 410 => UNAVAILABLE,
                429 => "音乐平台请求过于频繁，请稍后重试",
                300..=399 => FORMAT,
                _ => "音乐平台暂时不可用，请稍后重试",
            }
            .to_owned());
        }
        Ok(response.body)
    }

    fn json(&self, url: Url, referer: Option<&str>) -> Result<Value, String> {
        let body = self.body(&url, referer)?;
        serde_json::from_slice(&body).map_err(|_| FORMAT.to_owned())
    }
}

fn count(value: &Value) -> Option<usize> {
    id_number(value).and_then(|count| count.try_into().ok())
}

fn id_number(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        value
            .as_str()
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))?
            .parse()
            .ok()
    })
}

fn text(value: &Value) -> Option<String> {
    let s = value.as_str()?;
    let cleaned = s.split_whitespace().collect::<Vec<_>>().join(" ");
    (!cleaned.is_empty()).then_some(cleaned)
}

fn track(value: &Value, title: &str, artists: &str) -> Option<Track> {
    let title = text(&value[title])?;
    let artists = value[artists]
        .as_array()?
        .iter()
        .map(|artist| text(&artist["name"]))
        .collect::<Option<Vec<_>>>()?;
    Some(Track { title, artists })
}

fn netease(http: &Http<'_>, id: u64) -> Result<Playlist, String> {
    let meta = http.json(
        url_with_query(
            "https://music.163.com/api/v6/playlist/detail",
            &[("id", &id.to_string()), ("n", "100")],
        ),
        None,
    )?;
    netease_pages(meta, id, |ids| {
        let ids = serde_json::to_string(ids).expect("numeric track IDs serialize");
        http.json(
            url_with_query("https://music.163.com/api/song/detail", &[("ids", &ids)]),
            None,
        )
    })
}

fn netease_pages<F>(meta: Value, id: u64, mut get_songs: F) -> Result<Playlist, String>
where
    F: FnMut(&[u64]) -> Result<Value, String>,
{
    let p = &meta["playlist"];
    if meta["code"].as_u64() != Some(200)
        || id_number(&p["id"]) != Some(id)
        || p["privacy"].as_u64().is_some_and(|n| n != 0)
    {
        return Err(UNAVAILABLE.to_owned());
    }
    let mut list = Playlist::new(
        Platform::Netease,
        text(&p["name"]).ok_or_else(|| FORMAT.to_owned())?,
        count(&p["trackCount"]),
    );
    let inline = p["tracks"].as_array().ok_or_else(|| FORMAT.to_owned())?;
    let Some(manifest) = p["trackIds"].as_array() else {
        list.tracks.extend(
            inline
                .iter()
                .take(MAX_TRACKS)
                .filter_map(|t| track(t, "name", "ar")),
        );
        return list.finish(None);
    };
    if manifest.is_empty() && !inline.is_empty() {
        list.tracks.extend(
            inline
                .iter()
                .take(MAX_TRACKS)
                .filter_map(|t| track(t, "name", "ar")),
        );
        return list.finish(None);
    }
    let trimmed_end = manifest.len().min(MAX_TRACKS);
    let ids: Vec<_> = manifest[..trimmed_end]
        .iter()
        .map(|t| id_number(&t["id"]).filter(|&id| id != 0))
        .collect();
    let allowed: HashSet<_> = ids.iter().flatten().copied().collect();
    let mut songs: HashMap<_, _> = inline
        .iter()
        .filter_map(|t| {
            let id = id_number(&t["id"])?;
            allowed
                .contains(&id)
                .then(|| track(t, "name", "ar").map(|track| (id, track)))
                .flatten()
        })
        .collect();
    let mut seen = HashSet::new();
    let missing: Vec<_> = ids
        .iter()
        .flatten()
        .copied()
        .filter(|id| !songs.contains_key(id) && seen.insert(*id))
        .collect();
    let mut problem: Option<&'static str> = (manifest.len() > MAX_TRACKS).then_some(LIMIT);
    let mut detail_error: Option<String> = None;
    for batch in missing.chunks(PAGE_SIZE) {
        match get_songs(batch) {
            Ok(data) if data["code"].as_u64() == Some(200) && data["songs"].is_array() => {
                let response = data["songs"].as_array().expect("checked array");
                let mut received = HashSet::new();
                for song in response {
                    if let Some(id) = id_number(&song["id"]).filter(|id| batch.contains(id))
                        && let Some(track) = track(song, "name", "artists")
                    {
                        received.insert(id);
                        songs.insert(id, track);
                    }
                }
                if received.len() != batch.len() {
                    problem = Some(INCOMPLETE);
                    if received.is_empty() && !response.is_empty() {
                        problem = Some(CHANGED);
                        break;
                    }
                }
            }
            Ok(_) => {
                problem = Some(FORMAT);
                break;
            }
            Err(error) => {
                detail_error = Some(error);
                break;
            }
        }
    }
    for id in ids {
        if let Some(track) = id.and_then(|id| songs.get(&id)) {
            list.tracks.push(track.clone());
        } else if detail_error.is_none() {
            problem.get_or_insert(INCOMPLETE);
        }
    }
    if let Some(err) = detail_error
        && problem.is_none()
    {
        if list.tracks.is_empty() {
            return Err(err);
        }
        list.warning = Some(match list.expected_total {
            Some(total) => format!("{err}，已读取 {} / {total} 首", list.tracks.len()),
            None => format!("{err}，已读取 {} 首", list.tracks.len()),
        });
        return Ok(list);
    }
    list.finish(problem)
}

fn qq_url(id: u64, offset: usize) -> Url {
    url_with_query(
        "https://i.y.qq.com/qzone-music/fcg-bin/fcg_ucc_getcdinfo_byids_cp.fcg",
        &[
            ("type", "1"),
            ("json", "1"),
            ("utf8", "1"),
            ("onlysong", "0"),
            ("format", "json"),
            ("disstid", &id.to_string()),
            ("song_begin", &offset.to_string()),
            ("song_num", &PAGE_SIZE.to_string()),
        ],
    )
}

fn qq_manifest(page: &Value) -> Option<Vec<u64>> {
    let s = page["songids"].as_str()?;
    if s.is_empty() {
        return Some(Vec::new());
    }
    s.split(',').map(|s| s.parse().ok()).collect()
}

fn qq_pages<F>(id: u64, mut fetch: F) -> Result<Playlist, String>
where
    F: FnMut(usize) -> Result<Value, String>,
{
    let mut list: Option<Playlist> = None;
    let mut manifest = None;
    let mut previous_pages = HashSet::new();
    let mut offset = 0usize;
    let mut problem = None;
    let mut fetch_error: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let data = match fetch(offset) {
            Ok(data) => data,
            Err(error) => {
                fetch_error = Some(error);
                break;
            }
        };
        let p = &data["cdlist"][0];
        if data["code"].as_u64() != Some(0) || id_number(&p["disstid"]) != Some(id) {
            problem = Some(UNAVAILABLE);
            break;
        }
        let total = count(&p["total_song_num"]).or_else(|| count(&p["songnum"]));
        let page_manifest = qq_manifest(p);
        if let Some(first) = &list {
            if total != first.expected_total || page_manifest != manifest {
                problem = Some(CHANGED);
                break;
            }
        } else {
            manifest = page_manifest;
            list = Some(Playlist::new(
                Platform::Qq,
                text(&p["dissname"]).ok_or_else(|| FORMAT.to_owned())?,
                total,
            ));
        }
        let result = list.as_mut().expect("initialized on first page");
        let Some(songs) = p["songlist"].as_array() else {
            problem = Some(FORMAT);
            break;
        };
        let ids: Vec<_> = songs.iter().map(|s| id_number(&s["songid"])).collect();
        let end = offset + ids.len();
        if count(&p["song_begin"]) != Some(offset)
            || ids.len() > PAGE_SIZE
            || count(&p["cur_song_num"]) != Some(ids.len())
            || total.is_some_and(|total| end > total)
            || manifest.as_ref().is_some_and(|all| {
                all.get(offset..end).is_none_or(|expected| {
                    !ids.iter().copied().eq(expected.iter().copied().map(Some))
                })
            })
            || (manifest.is_none() && !ids.is_empty() && !previous_pages.insert(ids))
        {
            problem = Some(CHANGED);
            break;
        }
        for song in songs {
            if let Some(song) = track(song, "songname", "singer") {
                result.tracks.push(song);
            } else {
                problem = Some(INCOMPLETE);
            }
        }
        offset = end;
        if total == Some(offset) {
            if manifest.as_ref().is_some_and(|all| all.len() != offset) {
                problem = Some(INCOMPLETE);
            }
            return list.expect("initialized on first page").finish(problem);
        }
        if songs.is_empty() {
            problem = Some(INCOMPLETE);
            break;
        }
    }
    let Some(mut result) = list else {
        return Err(fetch_error.unwrap_or_else(|| problem.unwrap_or(FORMAT).to_owned()));
    };
    if let Some(err) = fetch_error
        && problem.is_none()
    {
        if result.tracks.is_empty() {
            return Err(err);
        }
        result.warning = Some(match result.expected_total {
            Some(total) => format!("{err}，已读取 {} / {total} 首", result.tracks.len()),
            None => format!("{err}，已读取 {} 首", result.tracks.len()),
        });
        return Ok(result);
    }
    result.finish(problem.or(Some(LIMIT)))
}

fn qishui(html: &[u8], id: u64) -> Result<Playlist, String> {
    let html = std::str::from_utf8(html).map_err(|_| FORMAT.to_owned())?;
    let raw = html
        .split_once(">_ROUTER_DATA = ")
        .ok_or_else(|| FORMAT.to_owned())?
        .1;
    let payload = serde_json::Deserializer::from_str(raw)
        .into_iter::<Value>()
        .next()
        .ok_or_else(|| FORMAT.to_owned())?
        .map_err(|_| FORMAT.to_owned())?;
    let page = &payload["loaderData"]["playlist_page"];
    let p = &page["playlistInfo"];
    let title = text(&p["title"])
        .filter(|title| title != "未知歌单")
        .ok_or_else(|| UNAVAILABLE.to_owned())?;
    if id_number(&p["id"]) != Some(id) || p["secret"].as_bool() == Some(true) {
        return Err(UNAVAILABLE.to_owned());
    }
    let medias = page["medias"].as_array().ok_or_else(|| FORMAT.to_owned())?;
    let mut total = count(&p["resource_cnt"]["track_cnt"]);
    let contains_videos = medias.iter().any(|m| m["type"].as_str() == Some("video"))
        || count(&p["resource_cnt"]["ugc_video_cnt"]).is_some_and(|n| n > 0);
    if !contains_videos {
        total = total
            .max(count(&p["count_tracks"]))
            .max(count(&p["stats"]["count_visible"]));
    }
    let mut list = Playlist::new(Platform::Qishui, title, total);
    let mut problem = None;
    for media in medias {
        if media["type"].as_str() == Some("video") {
            continue;
        }
        if list.tracks.len() == MAX_TRACKS {
            problem = Some(LIMIT);
            break;
        }
        if media["type"].as_str() == Some("track")
            && let Some(song) = track(&media["entity"]["track"], "name", "artists")
        {
            list.tracks.push(song);
        } else {
            problem = Some(INCOMPLETE);
        }
    }
    list.finish(problem)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn netease_uses_playlist_order_and_preserves_partial_song_details() {
        let meta = json!({"code": 200, "playlist": {
            "id": 3778678,
            "name": "热歌榜",
            "privacy": 0,
            "trackCount": 3,
            "trackIds": [{"id": 3}, {"id": 2}, {"id": 1}],
            "tracks": [{"id": 2, "name": "第二首", "ar": [{"name": "歌手"}]}]
        }});
        let result = netease_pages(meta, 3778678, |ids| {
            assert_eq!(ids, [3, 1]);
            Ok(json!({"code": 200, "songs": [{"id": 3, "name": "第一首", "artists": [{"name": "原唱"}]}]}))
        })
        .unwrap();
        assert_eq!(
            result.tracks.iter().map(Track::line).collect::<Vec<_>>(),
            ["第一首 - 原唱", "第二首 - 歌手"]
        );
        assert!(result.warning.unwrap().contains("2 / 3"));
    }

    #[test]
    fn qq_uses_pagination_and_keeps_multiple_singers() {
        let result = qq_pages(3602407677, |offset| {
            assert_eq!(offset, 0);
            Ok(json!({
                "code": 0,
                "cdlist": [{
                    "disstid": "3602407677",
                    "dissname": "QQ 公开歌单",
                    "total_song_num": 1,
                    "song_begin": 0,
                    "cur_song_num": 1,
                    "songids": "101",
                    "songlist": [
                        {"songid": 101, "songname": "合唱", "singer": [{"name": "甲"}, {"name": "乙"}]}
                    ]
                }]
            }))
        })
        .unwrap();
        assert_eq!(result.title, "QQ 公开歌单");
        assert_eq!(result.tracks[0].line(), "合唱 - 甲、乙");
        assert!(result.warning.is_none());
    }

    #[test]
    fn qishui_parses_router_data_and_skips_videos() {
        let html = r#"<html><script>_ROUTER_DATA = {"loaderData":{"playlist_page":{"playlistInfo":{"id":"7434476168507637799","title":"汽水精选","secret":false,"resource_cnt":{"track_cnt":1,"ugc_video_cnt":1},"count_tracks":2},"medias":[{"type":"video","entity":{}},{"type":"track","entity":{"track":{"name":"晴天","artists":[{"name":"歌手"}]}}}]}}};</script></html>"#;
        let result = qishui(html.as_bytes(), 7434476168507637799).unwrap();
        assert_eq!(result.platform, "汽水音乐");
        assert_eq!(result.title, "汽水精选");
        assert_eq!(result.expected_total, Some(1));
        assert_eq!(result.tracks[0].line(), "晴天 - 歌手");
        assert!(result.warning.is_none());
    }

    #[test]
    fn rejects_unsafe_addresses_and_invalid_redirects() {
        for ip in [
            "0.0.0.0",
            "10.0.0.5",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "172.31.0.1",
            "192.168.0.1",
            "198.18.1.1",
            "::",
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "2001:db8::1",
        ] {
            assert!(!public_ip(ip.parse().unwrap()), "{ip}");
        }
        assert!(public_ip("1.1.1.1".parse().unwrap()));
        let base = Url::parse("https://c6.y.qq.com/base/fcgi-bin/u?__=first").unwrap();
        assert!(
            redirect_source(
                &base,
                "http://i2.y.qq.com/n3/other/pages/details/playlist.html?id=10",
                Platform::Qq
            )
            .is_err()
        );
        assert!(
            redirect_source(&base, "https://music.163.com/playlist?id=10", Platform::Qq).is_err()
        );
        assert_eq!(
            redirect_source(
                &base,
                "//i2.y.qq.com/n3/other/pages/details/playlist.html?id=10",
                Platform::Qq
            ),
            Ok(Source::Playlist(Platform::Qq, 10))
        );
    }

    #[test]
    fn public_share_links_preserve_input_order_and_recognize_platforms() {
        let links = share_inputs(
            "歌单分享 https://music.163.com/#/playlist?id=123，http://y.qq.com/n/ryqq/playlist/456\n无效\nhttps://qishui.douyin.com/s/abcd123/",
        );
        assert_eq!(links.len(), 4);
        assert_eq!(
            recognize(&links[0]).unwrap(),
            Source::Playlist(Platform::Netease, 123)
        );
        assert_eq!(
            recognize(&links[1]).unwrap(),
            Source::Playlist(Platform::Qq, 456)
        );
        assert!(recognize(&links[2]).is_err());
        assert!(matches!(
            recognize(&links[3]).unwrap(),
            Source::Short(Platform::Qishui, _)
        ));
    }

    #[test]
    fn exports_partial_sources_in_order_and_preserves_repeated_tracks() {
        let track = track(
            &json!({"name": " 雨\n声 ", "ar": [{"name": "甲"}, {"name": "乙"}]}),
            "name",
            "ar",
        )
        .unwrap();
        let parsed = Playlist {
            platform: "网易云音乐",
            title: "收藏".into(),
            tracks: vec![track.clone(), track],
            expected_total: Some(3),
            warning: None,
        }
        .finish(None)
        .unwrap();
        assert_eq!(
            parsed.warning.as_deref(),
            Some("平台未返回全部曲目，已读取 2 / 3 首")
        );
        let results = [
            Outcome {
                input: "失败".into(),
                result: Err("无效".into()),
            },
            Outcome {
                input: "可读".into(),
                result: Ok(parsed),
            },
        ];
        assert_eq!(export_text(&results), "雨 声 - 甲、乙\n雨 声 - 甲、乙\n");
    }

    #[test]
    fn doh_extracts_first_public_ipv4_and_filters_private_addresses() {
        let body = serde_json::json!({
            "Answer": [
                {"type": 5, "data": "alias.example."},
                {"type": 1, "data": "127.0.0.1"},
                {"type": 1, "data": "198.18.0.75"},
                {"type": 1, "data": "117.149.204.165"}
            ]
        });
        assert_eq!(
            parse_doh_public_ip(body.to_string().as_bytes()),
            Some("117.149.204.165".parse().unwrap())
        );
    }
}
