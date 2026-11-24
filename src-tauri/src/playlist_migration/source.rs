use std::{io, net::IpAddr, sync::Arc, time::Duration};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{LOCATION, REFERER};
use reqwest::{Client, Response, Url, redirect::Policy};
use serde_json::Value;
use tokio::time::Instant;

use super::{FORMAT, Platform, Result, UNAVAILABLE, url_with_query};

const INVALID_URL: &str = "请提供有效的 HTTPS 歌单链接";
const UNSUPPORTED: &str = "不支持此链接，请复制网易云音乐、QQ 音乐或汽水音乐的歌单分享链接";
const REDIRECT: &str = "歌单短链接已失效，或跳转目标不是同一平台的歌单";
const NETWORK: &str = "无法连接音乐平台，请检查网络后重试";
const TIMEOUT: &str = "读取歌单超时，请稍后重试";
const MAX_REDIRECTS: usize = 4;
const MAX_URL: usize = 4096;
const MAX_BODY: usize = 8 * 1024 * 1024;
const REQUEST_TIME: Duration = Duration::from_secs(10);
const SOURCE_TIME: Duration = Duration::from_secs(45);

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Source {
    Playlist(Platform, u64),
    Short(Platform, Url),
}

fn valid_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.domain().is_some()
}

fn query(url: &Url, key: &str) -> Option<String> {
    let mut values = url.query_pairs().filter(|(name, _)| name == key);
    let (_, value) = values.next()?;
    values.next().is_none().then(|| value.into_owned())
}

fn playlist_id(value: &str) -> Option<u64> {
    if value.len() > 20 || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse().ok().filter(|&id| id != 0)
}

fn id_query(url: &Url, key: &str) -> Option<u64> {
    playlist_id(&query(url, key)?)
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

pub(super) fn recognize(raw: &str) -> Result<Source> {
    checked_url(raw.trim()).and_then(|url| classify(&url))
}

fn checked_url(raw: &str) -> Result<Url> {
    if raw.len() > MAX_URL
        || !raw.starts_with("https://")
        || raw.contains('\\')
        || raw.chars().any(char::is_control)
        || raw["https://".len()..]
            .split(['/', '?', '#'])
            .next()
            .is_none_or(|authority| authority.contains('@'))
    {
        return Err(INVALID_URL);
    }
    let url = Url::parse(raw).map_err(|_| INVALID_URL)?;
    valid_url(&url).then_some(url).ok_or(INVALID_URL)
}

fn classify(url: &Url) -> Result<Source> {
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
            let code = query(url, "__").filter(|s| token(s)).ok_or(UNSUPPORTED)?;
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

fn redirect_source(base: &Url, location: &str, platform: Platform) -> Result<Source> {
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

// Resolve through the OS, then hand only accepted addresses to reqwest's connector.
// There is no second DNS lookup and no system proxy that can resolve on our behalf.
struct PublicDns;

impl Resolve for PublicDns {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let addresses = tokio::net::lookup_host((name.as_str(), 0))
                .await?
                .filter(|addr| public_ip(addr.ip()))
                .collect::<Vec<_>>();
            if addresses.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "no public playlist endpoint addresses",
                )
                .into());
            }
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
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
            // Only global unicast; no IPv4-mapped, NAT64, 6to4, documentation or special-use ranges.
            (0x2000..=0x3fff).contains(&a)
                && !matches!((a, b), (0x2001, 0..=0x1ff | 0x0db8) | (0x2002 | 0x3fff, _))
        }
    }
}

pub(super) struct PublicHttp {
    client: Client,
    deadline: Instant,
}

impl PublicHttp {
    pub(super) fn new() -> Result<Self> {
        let client = Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(Policy::none())
            .dns_resolver(Arc::new(PublicDns))
            .connect_timeout(Duration::from_secs(5))
            .user_agent("TFY-BGM Public Playlist Migration")
            .build()
            .map_err(|_| NETWORK)?;
        Ok(Self {
            client,
            deadline: Instant::now() + SOURCE_TIME,
        })
    }

    async fn get(&self, url: Url, referer: Option<&str>) -> Result<Response> {
        let time = self
            .deadline
            .saturating_duration_since(Instant::now())
            .min(REQUEST_TIME);
        if time.is_zero() {
            return Err(TIMEOUT);
        }
        let mut request = self.client.get(url).timeout(time);
        if let Some(referer) = referer {
            request = request.header(REFERER, referer);
        }
        request
            .send()
            .await
            .map_err(|error| if error.is_timeout() { TIMEOUT } else { NETWORK })
    }

    pub(super) async fn resolve(&self, mut source: Source) -> Result<(Platform, u64)> {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..=MAX_REDIRECTS {
            match source {
                Source::Playlist(platform, id) => return Ok((platform, id)),
                Source::Short(platform, url) => {
                    if seen.len() >= MAX_REDIRECTS || !seen.insert(url.clone()) {
                        return Err(REDIRECT);
                    }
                    let response = self.get(url.clone(), None).await?;
                    let location = response
                        .headers()
                        .get(LOCATION)
                        .filter(|_| response.status().is_redirection())
                        .and_then(|v| v.to_str().ok())
                        .ok_or(REDIRECT)?;
                    source = redirect_source(&url, location, platform)?;
                }
            }
        }
        Err(REDIRECT)
    }

    pub(super) async fn body(&self, url: Url, referer: Option<&str>) -> Result<Vec<u8>> {
        let mut response = self.get(url, referer).await?;
        if !response.status().is_success() {
            return Err(match response.status().as_u16() {
                401 | 403 | 404 | 410 => UNAVAILABLE,
                429 => "音乐平台请求过于频繁，请稍后重试",
                300..=399 => FORMAT,
                _ => "音乐平台暂时不可用，请稍后重试",
            });
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_BODY as u64)
        {
            return Err("平台返回的歌单数据过大，无法读取");
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| if error.is_timeout() { TIMEOUT } else { NETWORK })?
        {
            if chunk.len() > MAX_BODY - body.len() {
                return Err("平台返回的歌单数据过大，无法读取");
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    pub(super) async fn json(&self, url: Url, referer: Option<&str>) -> Result<Value> {
        let body = self.body(url, referer).await?;
        serde_json::from_slice(&body).map_err(|_| FORMAT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_addresses() {
        for ip in [
            "0.0.0.0",
            "10.0.0.5",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "172.31.0.1",
            "192.168.0.1",
            "198.18.1.1",
            "224.1.0.1",
            "240.1.1.1",
            "::",
            "::1",
            "fc00::1",
            "fe80::1",
            "ff05::1",
            "::ffff:127.0.0.1",
            "64:ff9b::a00:1",
            "2001:db8::1",
            "2002:7f00:1::1",
            "3fff::1",
        ] {
            assert!(!public_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "1.1.1.1",
            "8.8.8.8",
            "2404:6800:4007::200e",
            "2606:4700::1111",
        ] {
            assert!(public_ip(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn validates_redirect_target_before_following_and_discards_tracking() {
        let base = Url::parse("https://c6.y.qq.com/base/fcgi-bin/u?__=first").unwrap();
        for url in [
            "http://i2.y.qq.com/n3/other/pages/details/playlist.html?id=10",
            "//127.0.0.1/",
            "https://i2.y.qq.com.evil.test/n3/other/pages/details/playlist.html?id=10",
            "https://music.163.com/playlist?id=10",
            "https://i2.y.qq.com/other?id=10",
            "https://i2.y.qq.com:444/n3/other/pages/details/playlist.html?id=10",
            "https://user@i2.y.qq.com/n3/other/pages/details/playlist.html?id=10",
        ] {
            assert!(redirect_source(&base, url, Platform::Qq).is_err(), "{url}");
        }
        assert_eq!(
            redirect_source(
                &base,
                "//i2.y.qq.com/n3/other/pages/details/playlist.html?id=10&hosteuin=unused",
                Platform::Qq
            ),
            Ok(Source::Playlist(Platform::Qq, 10))
        );
        let target = redirect_source(&base, "?__=second&unused=discard", Platform::Qq).unwrap();
        assert_eq!(
            target,
            Source::Short(
                Platform::Qq,
                Url::parse("https://c6.y.qq.com/base/fcgi-bin/u?__=second").unwrap()
            )
        );
    }
}
