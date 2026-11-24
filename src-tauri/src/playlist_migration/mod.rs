//! Anonymous, read-only playlist metadata. The IPC response never contains upstream URLs.

mod source;

use std::collections::{HashMap, HashSet};
use std::future::Future;

use reqwest::Url;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Semaphore;

use self::source::PublicHttp;
#[cfg(test)]
use self::source::Source;

const PAGE_SIZE: usize = 100;
const MAX_PAGES: usize = 30;
const MAX_TRACKS: usize = PAGE_SIZE * MAX_PAGES;
const UNAVAILABLE: &str = "歌单不存在、已失效或不是公开歌单，请检查分享链接";
const FORMAT: &str = "平台没有返回可读取的公开歌单数据，可能需要登录或链接已失效";
const INCOMPLETE: &str = "平台未返回全部曲目";
const CHANGED: &str = "歌单分页重复或顺序已变化，请重试";
const LIMIT: &str = "已达到单个歌单的读取上限";
type Result<T> = std::result::Result<T, &'static str>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Platform {
    Netease,
    Qq,
    Qishui,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Track {
    title: String,
    artists: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Playlist {
    platform: Platform,
    title: String,
    tracks: Vec<Track>,
    expected_total: Option<usize>,
    warning: Option<String>,
}

impl Playlist {
    fn new(platform: Platform, title: String, expected_total: Option<usize>) -> Self {
        Self {
            platform,
            title,
            tracks: Vec::new(),
            expected_total,
            warning: None,
        }
    }

    fn finish(mut self, problem: Option<&'static str>) -> Result<Self> {
        let actual = self.tracks.len();
        let problem = problem.or_else(|| match self.expected_total {
            Some(total) if total == actual => None,
            Some(_) => Some(INCOMPLETE),
            None => Some("平台未提供曲目总数，无法确认歌单是否完整"),
        });
        if let Some(reason) = problem {
            if actual == 0 {
                return Err(reason);
            }
            self.warning = Some(match self.expected_total {
                Some(total) => format!("{reason}，已读取 {actual} / {total} 首"),
                None => format!("{reason}，已读取 {actual} 首"),
            });
        }
        Ok(self)
    }
}

pub(crate) async fn parse(url: &str) -> std::result::Result<Playlist, String> {
    // A burst of IPC calls cannot queue an unbounded number of upstream jobs.
    static SLOTS: Semaphore = Semaphore::const_new(4);
    let source = source::recognize(url).map_err(str::to_owned)?;
    let _permit = SLOTS
        .try_acquire()
        .map_err(|_| "正在读取其他歌单，请稍后重试".to_owned())?;
    let http = PublicHttp::new().map_err(str::to_owned)?;
    let (platform, id) = http.resolve(source).await.map_err(str::to_owned)?;
    let result = match platform {
        Platform::Netease => netease(&http, id).await,
        Platform::Qq => {
            qq_pages(id, |offset| {
                http.json(qq_url(id, offset), Some("https://y.qq.com/"))
            })
            .await
        }
        Platform::Qishui => {
            let url = url_with_query(
                "https://music.douyin.com/qishui/share/playlist",
                &[("playlist_id", &id.to_string())],
            );
            match http.body(url, None).await {
                Ok(html) => qishui(&html, id),
                Err(error) => Err(error),
            }
        }
    };
    result.map_err(str::to_owned)
}

fn url_with_query(base: &str, query: &[(&str, &str)]) -> Url {
    let mut url = Url::parse(base).expect("static playlist endpoint");
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query.iter().copied());
    }
    url
}

fn count(value: &Value) -> Option<usize> {
    id_number(value).and_then(|count| count.try_into().ok())
}

fn id_number(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        value
            .as_str()
            .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))?
            .parse()
            .ok()
    })
}

fn text(value: &Value) -> Option<String> {
    let s = value.as_str()?;
    (!s.trim().is_empty()).then(|| s.to_owned())
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

async fn netease(http: &PublicHttp, id: u64) -> Result<Playlist> {
    let meta = http
        .json(
            url_with_query(
                "https://music.163.com/api/v6/playlist/detail",
                &[("id", &id.to_string()), ("n", "100")],
            ),
            None,
        )
        .await?;
    netease_pages(meta, id, |ids| {
        let ids = serde_json::to_string(ids).expect("numeric track IDs serialize");
        http.json(
            url_with_query("https://music.163.com/api/song/detail", &[("ids", &ids)]),
            None,
        )
    })
    .await
}

async fn netease_pages<F, Fut>(meta: Value, id: u64, mut get_songs: F) -> Result<Playlist>
where
    F: FnMut(&[u64]) -> Fut,
    Fut: Future<Output = Result<Value>>,
{
    let p = &meta["playlist"];
    if meta["code"].as_u64() != Some(200)
        || id_number(&p["id"]) != Some(id)
        || p["privacy"].as_u64().is_some_and(|n| n != 0)
    {
        return Err(UNAVAILABLE);
    }
    let mut list = Playlist::new(
        Platform::Netease,
        text(&p["name"]).ok_or(FORMAT)?,
        count(&p["trackCount"]),
    );
    let inline = p["tracks"].as_array().ok_or(FORMAT)?;
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
    let mut problem = (manifest.len() > MAX_TRACKS).then_some(LIMIT);
    for batch in missing.chunks(PAGE_SIZE) {
        match get_songs(batch).await {
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
                problem = Some(error);
                break;
            }
        }
    }
    for id in ids {
        if let Some(track) = id.and_then(|id| songs.get(&id)) {
            list.tracks.push(Track {
                title: track.title.clone(),
                artists: track.artists.clone(),
            });
        } else {
            problem.get_or_insert(INCOMPLETE);
        }
    }
    list.finish(problem)
}

fn qq_url(id: u64, offset: usize) -> Url {
    url_with_query(
        "https://c.y.qq.com/qzone/fcg-bin/fcg_ucc_getcdinfo_byids_cp.fcg",
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

async fn qq_pages<F, Fut>(id: u64, mut fetch: F) -> Result<Playlist>
where
    F: FnMut(usize) -> Fut,
    Fut: Future<Output = Result<Value>>,
{
    let mut list: Option<Playlist> = None;
    let mut manifest = None;
    let mut previous_pages = HashSet::new();
    let mut offset = 0usize;
    let mut problem = None;
    for _ in 0..MAX_PAGES {
        let data = match fetch(offset).await {
            Ok(data) => data,
            Err(error) => {
                problem = Some(error);
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
                text(&p["dissname"]).ok_or(FORMAT)?,
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
    list.ok_or_else(|| problem.unwrap_or(FORMAT))?
        .finish(problem.or(Some(LIMIT)))
}

fn qishui(html: &[u8], id: u64) -> Result<Playlist> {
    let html = std::str::from_utf8(html).map_err(|_| FORMAT)?;
    // Read the public share page's serialized router data, never evaluate its scripts.
    let raw = html.split_once(">_ROUTER_DATA = ").ok_or(FORMAT)?.1;
    let payload = serde_json::Deserializer::from_str(raw)
        .into_iter::<Value>()
        .next()
        .ok_or(FORMAT)?
        .map_err(|_| FORMAT)?;
    let page = &payload["loaderData"]["playlist_page"];
    let p = &page["playlistInfo"];
    let title = text(&p["title"])
        .filter(|title| title != "未知歌单")
        .ok_or(UNAVAILABLE)?;
    if id_number(&p["id"]) != Some(id) || p["secret"].as_bool() == Some(true) {
        return Err(UNAVAILABLE);
    }
    let medias = page["medias"].as_array().ok_or(FORMAT)?;
    // count_visible/count_tracks can include videos. Prefer the resource-specific song count.
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
mod tests;
