//! 智能随机权重、播放历史与失败跳过。

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use bgm_core::{KeyChange, Song};

const HISTORY_LIMIT: usize = 48;
const HISTORY_WINDOW: Duration = Duration::from_secs(45 * 60);

#[derive(Default)]
pub struct FailedTracks {
    keys: HashSet<String>,
}

impl FailedTracks {
    pub fn record(&mut self, key: String, max_skips: u32) -> bool {
        self.keys.insert(key) && self.keys.len() <= max_skips as usize
    }

    pub fn contains(&self, key: &str) -> bool {
        self.keys.contains(key)
    }

    pub fn clear(&mut self) {
        self.keys.clear();
    }
}

pub fn next_available_index(
    queue: &[String],
    current: usize,
    mut eligible: impl FnMut(&str) -> bool,
) -> Option<usize> {
    queue.get(current)?;
    (current + 1..queue.len())
        .chain(0..current)
        .find(|&index| eligible(&queue[index]))
}

#[derive(Default)]
pub struct PlayHistory {
    entries: VecDeque<(String, Instant)>,
}

impl PlayHistory {
    pub fn record(&mut self, key: String, now: Instant) {
        if self.entries.len() == HISTORY_LIMIT {
            self.entries.pop_front();
        }
        self.entries.push_back((key, now));
    }

    pub fn remap(&mut self, changes: &[KeyChange]) {
        for (key, _) in &mut self.entries {
            if let Some(change) = changes
                .iter()
                .find(|change| change.from == *key && change.to != *key)
            {
                key.clone_from(&change.to);
            }
        }
    }

    fn penalties(&self, now: Instant) -> HashMap<&str, f64> {
        let mut penalties = HashMap::new();
        for (index, (key, played_at)) in self.entries.iter().enumerate() {
            let age = now.saturating_duration_since(*played_at);
            if age > HISTORY_WINDOW {
                continue;
            }
            let recency = 1.0 - age.as_secs_f64() / HISTORY_WINDOW.as_secs_f64();
            let position = (index + 1) as f64 / self.entries.len() as f64;
            let value = recency * (0.45 + 0.55 * position);
            let penalty = penalties.entry(key.as_str()).or_insert(0.0_f64);
            *penalty = penalty.max(value);
        }
        penalties
    }
}

pub fn pick<'a>(
    candidates: impl Iterator<Item = Option<&'a Song>>,
    current: Option<&Song>,
    favorites: &[String],
    history: &PlayHistory,
) -> Option<usize> {
    let favorites = favorites.iter().map(String::as_str).collect();
    let recent = history.penalties(Instant::now());
    let weights = smart_weights(candidates, current, &favorites, &recent);
    weighted_index(&weights, fastrand::f64())
}

fn smart_weights<'a>(
    candidates: impl Iterator<Item = Option<&'a Song>>,
    current: Option<&Song>,
    favorites: &HashSet<&str>,
    recent: &HashMap<&str, f64>,
) -> Vec<f64> {
    let current_artist = current
        .and_then(|song| song.artist.as_ref())
        .map(|tag| tag.to_lowercase());
    let current_album = current
        .and_then(|song| song.album.as_ref())
        .map(|tag| tag.to_lowercase());
    let current_genre = current
        .and_then(|song| song.genre.as_deref())
        .filter(|tag| !tag.is_empty());
    let current_duration = current
        .and_then(|song| song.duration_ms)
        .filter(|&duration| duration > 0);
    let mut has_alternative = false;

    let scores: Vec<_> = candidates
        .map(|item| {
            let Some(song) = item else {
                return (0.0, false);
            };
            let artist_matches =
                song.artist.as_ref().map(|tag| tag.to_lowercase()) == current_artist;
            let album_matches = song.album.as_ref().map(|tag| tag.to_lowercase()) == current_album;
            has_alternative |= !artist_matches && !album_matches;
            let same_artist =
                artist_matches && current_artist.as_ref().is_some_and(|tag| !tag.is_empty());
            let same_album =
                album_matches && current_album.as_ref().is_some_and(|tag| !tag.is_empty());

            let mut weight = if current.is_some_and(|item| item.category == song.category) {
                1.18
            } else {
                0.95
            };
            if favorites.contains(song.key.as_str()) {
                weight *= 1.9;
            }
            if current_genre.is_some() && current_genre == song.genre.as_deref() {
                weight *= 1.08;
            }
            weight *= (1.0 - recent.get(song.key.as_str()).copied().unwrap_or_default()).max(0.05);
            if let (Some(current_ms), Some(song_ms)) = (
                current_duration,
                song.duration_ms.filter(|&duration| duration > 0),
            ) {
                weight *= match (current_ms, song_ms) {
                    (180_000.., ..90_000) => 0.22,
                    (..60_000, ..60_000) => 0.4,
                    (300_000.., 300_000..) => 0.75,
                    _ => 1.0,
                };
            }
            if same_album {
                weight *= 0.03;
            } else if same_artist {
                weight *= 0.05;
            }
            (weight, same_artist || same_album)
        })
        .collect();

    scores
        .into_iter()
        .map(|(weight, related)| {
            if has_alternative && related {
                0.0
            } else {
                weight
            }
        })
        .collect()
}

fn weighted_index(weights: &[f64], unit_roll: f64) -> Option<usize> {
    let total: f64 = weights.iter().sum();
    if total <= 0.0 {
        return None;
    }
    let mut roll = unit_roll * total;
    for (index, &weight) in weights.iter().enumerate() {
        roll -= weight;
        if roll <= 0.0 && weight > 0.0 {
            return Some(index);
        }
    }
    weights.iter().position(|&weight| weight > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moving_a_recent_song_keeps_its_original_history_penalty() {
        let now = Instant::now();
        let mut history = PlayHistory::default();
        history.record("epic/song".into(), now - Duration::from_secs(60));
        let penalty = history.penalties(now)["epic/song"];
        history.remap(&[KeyChange {
            from: "epic/song".into(),
            to: "gufeng/song".into(),
        }]);
        let penalties = history.penalties(now);
        assert!(!penalties.contains_key("epic/song"));
        assert_eq!(penalties["gufeng/song"], penalty);
    }

    fn song(key: &str, artist: &str, album: &str, duration_ms: Option<u32>) -> Song {
        Song {
            key: key.into(),
            title: key.into(),
            file_name: format!("{key}.mp3"),
            path: String::new(),
            category: "epic".into(),
            artist: Some(artist.into()),
            album: Some(album.into()),
            genre: None,
            duration_ms,
        }
    }

    #[test]
    fn recovery_excludes_failed_keys_in_both_play_modes() {
        let queue = ["a", "b", "a", "c"].map(str::to_owned);
        let mut failed = FailedTracks::default();
        assert!(failed.record("a".to_owned(), 8));
        assert!(failed.record("b".to_owned(), 8));
        let untried = |key: &str| !failed.contains(key);
        assert_eq!(next_available_index(&queue, 1, untried), Some(3));
        assert_eq!(next_available_index(&queue, 3, untried), None);
        assert_eq!(next_available_index(&[], 0, untried), None);

        let songs = [
            song("a", "X", "I", None),
            song("b", "Y", "J", None),
            song("c", "Y", "J", None),
        ];
        let current = Some(&songs[1]);
        let candidates = queue
            .iter()
            .map(|key| songs.iter().find(|song| &song.key == key && untried(key)));
        assert_eq!(
            pick(candidates, current, &[], &PlayHistory::default()),
            Some(3)
        );

        assert!(failed.record("c".to_owned(), 8));
        assert!(!failed.record("a".to_owned(), 8));
        let untried = |key: &str| !failed.contains(key);
        assert_eq!(next_available_index(&queue, 3, untried), None);
        let candidates = songs.iter().map(|song| untried(&song.key).then_some(song));
        assert_eq!(
            pick(candidates, current, &[], &PlayHistory::default()),
            None
        );
    }

    #[test]
    fn weight_roll_reaches_the_end_and_skips_zero_weight_entries() {
        let weights = [0.0, 1.0, 0.0, 3.0];
        assert_eq!(weighted_index(&weights, 0.0), Some(1));
        assert_eq!(weighted_index(&weights, 0.25), Some(1));
        assert_eq!(weighted_index(&weights, 0.251), Some(3));
        assert_eq!(weighted_index(&weights, 0.999), Some(3));
        assert_eq!(weighted_index(&[0.0, 0.0], 0.5), None);
    }

    #[test]
    fn alternative_filters_artist_and_album_regardless_of_case() {
        let current = song("playing", "Écho", "Night", None);
        let artist = song("artist", "éCHO", "Morning", None);
        let album = song("album", "Other", "night", None);
        let different = song("different", "Other", "Morning", None);
        let favorites = HashSet::from(["album"]);
        let recent = HashMap::new();
        let enough = [None, Some(&artist), Some(&album), Some(&different)];
        assert_eq!(
            smart_weights(enough.into_iter(), Some(&current), &favorites, &recent),
            [0.0, 0.0, 0.0, 1.18]
        );

        let related = [Some(&artist), Some(&album)];
        let weights = smart_weights(related.into_iter(), Some(&current), &favorites, &recent);
        assert!((weights[0] - 1.18 * 0.05).abs() < 1e-9);
        assert!((weights[1] - 1.18 * 1.9 * 0.03).abs() < 1e-9);
    }

    #[test]
    fn zero_and_missing_durations_skip_length_weights() {
        let current = song("playing", "A", "X", None);
        let candidates = [
            song("short", "B", "Y", Some(30_000)),
            song("zero", "B", "Z", Some(0)),
        ];
        let weights = smart_weights(
            candidates.iter().map(Some),
            Some(&current),
            &HashSet::new(),
            &HashMap::new(),
        );
        assert_eq!(weights, [1.18, 1.18]);
    }

    #[test]
    fn duration_boundaries_and_preference_weights_match_the_contract() {
        let mut current = song("playing", "A", "X", Some(300_000));
        current.genre = Some("ambient".into());
        let mut short = song("short", "B", "Y", Some(89_999));
        short.genre = current.genre.clone();
        let edge = song("edge", "C", "Z", Some(90_000));
        let long = song("long", "D", "W", Some(300_000));
        let favorites = HashSet::from(["short"]);
        let recent = HashMap::from([("short", 1.0)]);
        let weights = smart_weights(
            [Some(&short), Some(&edge), Some(&long)].into_iter(),
            Some(&current),
            &favorites,
            &recent,
        );
        assert!((weights[0] - 1.18 * 1.9 * 1.08 * 0.05 * 0.22).abs() < 1e-9);
        assert_eq!(weights[1], 1.18);
        assert_eq!(weights[2], 1.18 * 0.75);

        current.duration_ms = Some(59_999);
        short.duration_ms = Some(59_999);
        let weights = smart_weights(
            [Some(&short), Some(&edge)].into_iter(),
            Some(&current),
            &HashSet::new(),
            &HashMap::new(),
        );
        assert_eq!(weights, [1.18 * 1.08 * 0.4, 1.18]);
    }

    #[test]
    fn history_keeps_48_records_and_ignores_plays_beyond_45_minutes() {
        let now = Instant::now() + Duration::from_secs(60 * 60);
        let mut history = PlayHistory::default();
        for index in 0..49 {
            history.record(index.to_string(), now - Duration::from_secs(2700 - index));
        }
        let penalties = history.penalties(now);
        assert_eq!(penalties.len(), 48);
        assert!(!penalties.contains_key("0"));
        assert!(penalties["48"] > penalties["1"]);

        let later = now + Duration::from_secs(2);
        let penalties = history.penalties(later);
        assert!(!penalties.contains_key("1"));
        assert_eq!(penalties["2"], 0.0);
    }
}
