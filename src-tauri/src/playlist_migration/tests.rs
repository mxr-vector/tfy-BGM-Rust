use std::future::ready;

use serde_json::json;

use super::*;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn recognizes_platform_urls_and_strips_share_parameters() {
    for (url, platform, id) in [
        (
            "https://music.163.com/playlist?id=42&userid=23",
            Platform::Netease,
            42,
        ),
        (
            "https://music.163.com/#/playlist?id=42",
            Platform::Netease,
            42,
        ),
        ("https://music.163.com/playlist/42", Platform::Netease, 42),
        (
            "https://y.music.163.com/m/playlist?id=42",
            Platform::Netease,
            42,
        ),
        ("https://y.qq.com/n/ryqq/playlist/42", Platform::Qq, 42),
        ("https://y.qq.com/n/yqq/playlist/42.html", Platform::Qq, 42),
        (
            "https://i.y.qq.com/n2/m/share/details/taoge.html?id=42",
            Platform::Qq,
            42,
        ),
        (
            "https://i2.y.qq.com/n3/other/pages/details/playlist.html?id=42",
            Platform::Qq,
            42,
        ),
        (
            "https://music.douyin.com/qishui/share/playlist?playlist_id=42&sec_sharer_id=unused",
            Platform::Qishui,
            42,
        ),
        (
            "https://www.qishui.com/share/playlist?playlist_id=42",
            Platform::Qishui,
            42,
        ),
        ("https://www.qishui.com/playlist/42", Platform::Qishui, 42),
    ] {
        assert_eq!(
            source::recognize(url),
            Ok(Source::Playlist(platform, id)),
            "{url}"
        );
    }
    for (raw, platform, expected) in [
        (
            "https://163cn.tv/bjELbBPn?unused=1",
            Platform::Netease,
            "https://163cn.tv/bjELbBPn",
        ),
        (
            "https://c6.y.qq.com/base/fcgi-bin/u?__=6RqkIPxAqtIb&unused=1",
            Platform::Qq,
            "https://c6.y.qq.com/base/fcgi-bin/u?__=6RqkIPxAqtIb",
        ),
        (
            "https://qishui.douyin.com/s/ixfhfRe9?unused=1",
            Platform::Qishui,
            "https://qishui.douyin.com/s/ixfhfRe9/",
        ),
    ] {
        assert_eq!(
            source::recognize(raw),
            Ok(Source::Short(platform, Url::parse(expected).unwrap()))
        );
    }
}

#[test]
fn rejects_non_playlist_and_untrusted_urls() {
    for url in [
        "http://music.163.com/playlist?id=42",
        "https://music.163.com.evil.test/playlist?id=42",
        "https://music.163.com@127.0.0.1/playlist?id=42",
        "https://@music.163.com/playlist?id=42",
        "https://music.163.com.:443/playlist?id=42",
        "https://music.163.com:444/playlist?id=42",
        "https://localhost/playlist?id=42",
        "https://[::1]/playlist?id=42",
        "https://127.1/playlist?id=42",
        "https://music.163.com/song?id=42",
        "https://music.163.com/other#/playlist?id=42",
        "https://music.163.com/playlist?id=42&id=43",
        "https://music.163.com/playlist?id=42%26n%3D99",
        "https://music.163.com/playlist?id=18446744073709551616",
        "https://music.163.com/playlist?id=0",
        "https://y.qq.com/n/ryqq/songDetail/42",
        "https://qishui.douyin.com/s/token/other",
        "https://music.douyin.com/qishui/share/track?track_id=42",
        "https://163cn.tv/%2Fbad",
        "https://c6.y.qq.com/base/fcgi-bin/u?__=first&__=second",
    ] {
        assert!(source::recognize(url).is_err(), "{url}");
    }
}

fn netease_fixture(n: usize) -> Value {
    json!({"code": 200, "playlist": {"id": 42, "name": "测试", "trackCount": n,
        "trackIds": (1..=n).map(|id| json!({"id": id})).collect::<Vec<_>>(),
        "tracks": if n == 0 { vec![] } else { vec![json!({"id": 1, "name": "首曲 (Live)", "ar": [{"name":"甲"}, {"name":"乙"}]})] }
    }})
}

fn netease_songs(ids: &[u64]) -> Value {
    json!({"code":200,"songs":ids.iter().rev().map(|id| json!({"id":id,"name":format!("曲{id}"),"artists":[{"name":"甲"}]})).collect::<Vec<_>>()})
}

#[test]
fn netease_batches_manifest_preserving_order_and_inline_tracks() {
    let mut offsets = Vec::new();
    let result = runtime()
        .block_on(netease_pages(netease_fixture(104), 42, |ids| {
            offsets.push((ids[0], ids.len()));
            ready(Ok(netease_songs(ids)))
        }))
        .unwrap();
    assert_eq!(offsets, [(2, 100), (102, 3)]);
    assert_eq!(result.tracks.len(), 104);
    assert_eq!(result.tracks[0].artists, ["甲", "乙"]);
    assert_eq!(result.tracks[0].title, "首曲 (Live)");
    assert_eq!(result.tracks[1].title, "曲2");
    assert_eq!(result.tracks.last().unwrap().title, "曲104");
    assert_eq!(result.warning, None);
    let result = serde_json::to_value(result).unwrap();
    assert_eq!(result["platform"], "netease");
    assert_eq!(result["expectedTotal"], 104);
    assert!(result["warning"].is_null());
}

#[test]
fn netease_retains_partial_and_distinguishes_private_and_empty() {
    let runtime = runtime();
    let result = runtime
        .block_on(netease_pages(netease_fixture(105), 42, |ids| {
            ready(if ids[0] == 2 {
                Ok(netease_songs(&ids[1..]))
            } else {
                Err("请求超时")
            })
        }))
        .unwrap();
    assert_eq!(result.tracks.len(), 100);
    assert!(result.warning.unwrap().contains("100 / 105"));
    let empty = runtime
        .block_on(netease_pages(netease_fixture(0), 42, |_| {
            ready(Err("不应读取"))
        }))
        .unwrap();
    assert!(empty.tracks.is_empty() && empty.warning.is_none());
    let mut private = netease_fixture(2);
    private["playlist"]["privacy"] = json!(10);
    assert_eq!(
        runtime
            .block_on(netease_pages(private, 42, |_| ready(Err("不应读取"))))
            .unwrap_err(),
        UNAVAILABLE
    );
}

#[test]
fn netease_checks_inline_total_when_manifest_is_absent_or_empty() {
    let runtime = runtime();
    for manifest in [Value::Null, json!([])] {
        let mut complete = netease_fixture(1);
        complete["playlist"]["trackIds"] = manifest;
        let mut incomplete = complete.clone();
        incomplete["playlist"]["trackCount"] = json!(2);
        let read =
            |fixture| runtime.block_on(netease_pages(fixture, 42, |_| ready(Err(INCOMPLETE))));
        assert!(read(complete).unwrap().warning.is_none());
        assert!(read(incomplete).unwrap().warning.unwrap().contains("1 / 2"));
    }
}

fn qq_fixture(offset: usize, ids: &[u64], manifest: Option<&str>, total: usize) -> Value {
    json!({"code":0,"cdlist":[{"disstid":"42","dissname":"测试","total_song_num":total,"song_begin":offset,
        "cur_song_num":ids.len(),"songids":manifest,"songlist":ids.iter().map(|id| json!({"songid":id,"songname":format!("曲{id}"),"singer":[{"name":"甲"},{"name":"乙"}]})).collect::<Vec<_>>()
    }]})
}

#[test]
fn qq_reads_short_pages_in_order_even_with_an_intentional_repeat() {
    let mut offsets = Vec::new();
    let result = runtime()
        .block_on(qq_pages(42, |offset| {
            offsets.push(offset);
            ready(Ok(qq_fixture(
                offset,
                if offset == 0 { &[1, 2] } else { &[1] },
                Some("1,2,1"),
                3,
            )))
        }))
        .unwrap();
    assert_eq!(offsets, [0, 2]);
    assert_eq!(
        result
            .tracks
            .iter()
            .map(|t| t.title.as_str())
            .collect::<Vec<_>>(),
        ["曲1", "曲2", "曲1"]
    );
    assert_eq!(result.warning, None);
}

#[test]
fn qq_retains_good_pages_on_repetition_offset_failure_and_truncation() {
    let runtime = runtime();
    for mode in ["repeat", "offset", "network", "truncated", "wrong_order"] {
        let manifest = (mode == "wrong_order").then_some("1,2,3");
        let result = runtime
            .block_on(qq_pages(42, |offset| {
                ready(match (offset, mode) {
                    (0, _) => Ok(qq_fixture(0, &[1], manifest, 3)),
                    (_, "repeat") => Ok(qq_fixture(offset, &[1], None, 3)),
                    (_, "offset") => Ok(qq_fixture(0, &[2], None, 3)),
                    (_, "truncated") => Ok(qq_fixture(offset, &[], None, 3)),
                    (_, "wrong_order") => Ok(qq_fixture(offset, &[3], manifest, 3)),
                    _ => Err("请求超时"),
                })
            }))
            .unwrap();
        assert_eq!(result.tracks.len(), 1, "{mode}");
        assert!(result.warning.unwrap().contains("1 / 3"), "{mode}");
    }
    assert!(
        runtime
            .block_on(qq_pages(42, |_| ready(Err("请求超时"))))
            .is_err()
    );
}

#[test]
fn qq_stops_at_page_limit() {
    let mut calls = 0;
    let result = runtime()
        .block_on(qq_pages(42, |offset| {
            calls += 1;
            let ids = (offset as u64..offset as u64 + PAGE_SIZE as u64).collect::<Vec<_>>();
            ready(Ok(qq_fixture(offset, &ids, None, MAX_TRACKS + 1)))
        }))
        .unwrap();
    assert_eq!(calls, MAX_PAGES);
    assert_eq!(result.tracks.len(), MAX_TRACKS);
    assert!(result.warning.unwrap().contains(LIMIT));
}

#[test]
fn qishui_reads_structured_public_html_without_evaluating_scripts() {
    let payload = json!({"loaderData":{"playlist_page":{
        "playlistInfo":{"id":"42","title":"测试","count_tracks":3,"stats":{"count_visible":4},"resource_cnt":{"track_cnt":2,"ugc_video_cnt":2}},
        "medias":[{"type":"video"},{"type":"track","entity":{"track":{"name":"歌;曲 (Live)","artists":[{"name":"甲"},{"name":"乙"}]}}}]
    }}});
    let html =
        format!("<script nonce=\"public\">_ROUTER_DATA = {payload}; doNotExecute()</script>");
    let result = qishui(html.as_bytes(), 42).unwrap();
    assert_eq!(result.tracks[0].title, "歌;曲 (Live)");
    assert_eq!(result.tracks[0].artists, ["甲", "乙"]);
    assert_eq!(result.expected_total, Some(2));
    assert!(result.warning.unwrap().contains("1 / 2"));
    assert!(qishui(html.as_bytes(), 43).is_err());
    assert!(qishui(b"please log in", 42).is_err());
}

#[test]
#[ignore = "makes anonymous requests to public sample playlists"]
fn live_public_samples() {
    runtime().block_on(async {
        let mut failed = Vec::new();
        for (name, url) in [
            (
                "NetEase web",
                "https://music.163.com/playlist?id=2791573169",
            ),
            (
                "NetEase hash",
                "https://music.163.com/#/playlist?id=2791573169",
            ),
            ("NetEase short", "https://163cn.tv/bjELbBPn"),
            ("QQ web", "https://y.qq.com/n/ryqq/playlist/7719884207"),
            (
                "QQ mobile",
                "https://i.y.qq.com/n2/m/share/details/taoge.html?id=7719884207",
            ),
            (
                "QQ short",
                "https://c6.y.qq.com/base/fcgi-bin/u?__=6RqkIPxAqtIb",
            ),
            (
                "Qishui share",
                "https://music.douyin.com/qishui/share/playlist?playlist_id=7270371377822253096",
            ),
            ("Qishui short", "https://qishui.douyin.com/s/ixfhfRe9/"),
            (
                "Qishui full share",
                "https://music.douyin.com/qishui/share/playlist?playlist_id=7656740455992131636",
            ),
            (
                "Qishui full web",
                "https://www.qishui.com/playlist/7656740455992131636",
            ),
            (
                "NetEase missing",
                "https://music.163.com/playlist?id=99999999999999",
            ),
            ("NetEase expired", "https://163cn.tv/NotARealLink"),
            (
                "QQ expired",
                "https://c6.y.qq.com/base/fcgi-bin/u?__=Ix01hzh0Geg5",
            ),
            ("Qishui expired", "https://qishui.douyin.com/s/ixRUS9BD/"),
            (
                "Qishui missing",
                "https://music.douyin.com/qishui/share/playlist?playlist_id=7397707424637239332",
            ),
        ] {
            let expect_error = name.ends_with("missing") || name.ends_with("expired");
            match parse(url).await {
                Ok(list) => {
                    println!(
                        "{name}: {} | {}/{:?} | {:?} -> {:?} | {:?}",
                        list.title,
                        list.tracks.len(),
                        list.expected_total,
                        list.tracks.first(),
                        list.tracks.last(),
                        list.warning
                    );
                    let expected_partial = matches!(name, "Qishui share" | "Qishui short");
                    if expect_error
                        || list.tracks.is_empty()
                        || list.warning.is_some() != expected_partial
                    {
                        failed.push(name);
                    }
                }
                Err(error) => {
                    println!("{name}: {error}");
                    if !expect_error {
                        failed.push(name);
                    }
                }
            }
        }
        assert!(failed.is_empty(), "unexpected sample results: {failed:?}");
    });
}
