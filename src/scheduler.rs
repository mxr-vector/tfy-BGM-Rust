//! 节目单触发、回看和本地时间对齐。

use std::collections::HashSet;

use bgm_core::{ScheduleEntry, ScheduleSong};
use chrono::{DateTime, Local, NaiveDate, TimeZone, Timelike};

const CATCHUP_MS: i64 = 120_000;
const WAKE_GAP_MS: i64 = 90_000;

#[derive(Clone, Copy)]
pub enum Check {
    Tick { previous: Option<DateTime<Local>> },
    Edited,
    TrackRemoved,
}

#[derive(Default)]
pub struct Scheduler {
    day: Option<NaiveDate>,
    active_id: Option<String>,
    triggered: HashSet<(String, String, String)>,
}

pub struct Update<'a> {
    pub entry: &'a ScheduleEntry,
    pub song: Option<&'a ScheduleSong>,
    pub switched: bool,
}

impl Scheduler {
    pub fn check<'a>(
        &mut self,
        entries: &'a [ScheduleEntry],
        now: DateTime<Local>,
        check: Check,
        available: impl Fn(&str) -> bool,
    ) -> Option<Update<'a>> {
        let day = now.date_naive();
        if self.day != Some(day) {
            self.day = Some(day);
            self.active_id = None;
            self.triggered.clear();
        }
        let minute = now.hour() * 60 + now.minute();
        let Some(entry) = active_program(entries, minute) else {
            self.active_id = None;
            return None;
        };
        let switched = self.active_id.as_deref() != Some(entry.id.as_str());
        if switched {
            self.active_id = Some(entry.id.clone());
        }

        let edited = matches!(check, Check::Edited);
        let realign = switched || edited || matches!(check, Check::TrackRemoved);
        let resume = matches!(check, Check::Tick { previous } if previous.is_none_or(|then| {
            let gap = (now - then).num_milliseconds();
            !(0..=WAKE_GAP_MS).contains(&gap)
        }));
        let align = realign || resume;
        let due = latest_due_song(entry, minute, &available);
        let mut new_due = false;
        for song in &entry.songs {
            let Some(start) = minutes_of(&song.start).filter(|&start| start <= minute) else {
                continue;
            };
            if !align {
                let age = day
                    .and_hms_opt(start / 60, start % 60, 0)
                    .and_then(|start| Local.from_local_datetime(&start).earliest())
                    .map(|start| (now - start).num_milliseconds());
                if !age.is_some_and(|age| (0..=CATCHUP_MS).contains(&age)) {
                    continue;
                }
            }
            let trigger = (entry.id.clone(), song.start.clone(), song.key.clone());
            let new = self.triggered.insert(trigger);
            new_due |= new && due.is_some_and(|target| std::ptr::eq(target, song));
        }

        if !(realign || new_due || (resume && due.is_some())) {
            return None;
        }
        let song = if edited {
            due.or_else(|| entry.songs.iter().find(|song| available(&song.key)))
        } else {
            due
        };
        Some(Update {
            entry,
            song,
            switched,
        })
    }
}

pub fn active_program(entries: &[ScheduleEntry], minute: u32) -> Option<&ScheduleEntry> {
    entries.iter().find(|entry| {
        entry.enabled
            && minutes_of(&entry.start).is_some_and(|start| start <= minute)
            && minutes_of(&entry.end).is_some_and(|end| minute < end)
    })
}

pub fn latest_due_song(
    entry: &ScheduleEntry,
    minute: u32,
    available: impl Fn(&str) -> bool,
) -> Option<&ScheduleSong> {
    entry
        .songs
        .iter()
        .rev()
        .filter_map(|song| {
            let start = minutes_of(&song.start)?;
            (start <= minute && available(&song.key)).then_some((start, song))
        })
        .max_by_key(|(start, _)| *start)
        .map(|(_, song)| song)
}

pub fn minutes_of(time: &str) -> Option<u32> {
    let (hour, minute) = time.trim().split_once(':')?;
    let hour: u32 = hour.trim().parse().ok()?;
    let minute: u32 = minute.trim().parse().ok()?;
    (hour <= 23 && minute <= 59).then_some(hour * 60 + minute)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn time(day: u32, hour: u32, minute: u32, second: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 11, day, hour, minute, second)
            .single()
            .unwrap()
    }

    fn program(id: &str, start: &str, end: &str, songs: &[(&str, &str)]) -> ScheduleEntry {
        ScheduleEntry {
            id: id.into(),
            start: start.into(),
            end: end.into(),
            songs: songs
                .iter()
                .map(|(key, start)| ScheduleSong {
                    key: (*key).into(),
                    start: (*start).into(),
                    end: end.into(),
                })
                .collect(),
            ..ScheduleEntry::default()
        }
    }

    #[test]
    fn catchup_applies_only_the_latest_start_and_consumes_earlier_events() {
        let entries = [program(
            "morning",
            "09:58",
            "11:00",
            &[("old", "10:00"), ("new", "10:01")],
        )];
        let mut scheduler = Scheduler::default();
        let before = time(19, 9, 59, 57);
        let after = time(19, 10, 1, 5);
        let start = scheduler
            .check(&entries, before, Check::Tick { previous: None }, |_| true)
            .unwrap();
        assert!(start.song.is_none());
        let caught_up = scheduler
            .check(
                &entries,
                after,
                Check::Tick {
                    previous: Some(before),
                },
                |_| true,
            )
            .unwrap();
        assert_eq!(caught_up.song.unwrap().key, "new");
        assert!(
            scheduler
                .check(
                    &entries,
                    after + Duration::seconds(1),
                    Check::Tick {
                        previous: Some(after)
                    },
                    |key| key != "new"
                )
                .is_none()
        );
    }

    #[test]
    fn recovery_chooses_a_due_song_beyond_the_catchup_window() {
        let entries = [program(
            "morning",
            "09:00",
            "11:00",
            &[("old", "09:00"), ("missing", "09:08"), ("next", "10:00")],
        )];
        let mut scheduler = Scheduler::default();
        let before = time(19, 9, 0, 0);
        let after = time(19, 9, 30, 0);
        scheduler
            .check(&entries, before, Check::Tick { previous: None }, |_| true)
            .unwrap();
        let recovered = scheduler
            .check(
                &entries,
                after,
                Check::Tick {
                    previous: Some(before),
                },
                |key| key != "missing",
            )
            .unwrap();
        assert_eq!(recovered.song.unwrap().key, "old");
        assert!(!recovered.switched);
        assert!(
            scheduler
                .check(
                    &entries,
                    after + Duration::seconds(1),
                    Check::Tick {
                        previous: Some(after)
                    },
                    |key| key != "missing"
                )
                .is_none()
        );
    }

    #[test]
    fn removing_the_current_track_realigns_without_starting_a_future_song() {
        let entries = [program(
            "morning",
            "09:00",
            "11:00",
            &[
                ("previous", "09:00"),
                ("removed", "09:15"),
                ("next", "10:00"),
            ],
        )];
        let mut scheduler = Scheduler::default();
        let now = time(19, 9, 30, 0);
        let initial = scheduler
            .check(&entries, now, Check::Tick { previous: None }, |_| true)
            .unwrap();
        assert_eq!(initial.song.unwrap().key, "removed");

        let fallback = scheduler
            .check(&entries, now, Check::TrackRemoved, |key| key != "removed")
            .unwrap();
        assert_eq!(fallback.song.unwrap().key, "previous");
        assert!(!fallback.switched);

        let waiting = scheduler
            .check(&entries, now, Check::TrackRemoved, |key| key == "next")
            .unwrap();
        assert!(waiting.song.is_none());

        let next = time(19, 10, 0, 0);
        let update = scheduler
            .check(
                &entries,
                next,
                Check::Tick {
                    previous: Some(next - Duration::seconds(1)),
                },
                |key| key == "next",
            )
            .unwrap();
        assert_eq!(update.song.unwrap().key, "next");
    }

    #[test]
    fn editing_starts_the_first_available_song_before_its_slot() {
        let entries = [program(
            "morning",
            "09:00",
            "11:00",
            &[("missing", "09:05"), ("first", "09:08")],
        )];
        let mut scheduler = Scheduler::default();
        let before = time(19, 9, 0, 0);
        let start = scheduler
            .check(&entries, before, Check::Tick { previous: None }, |_| true)
            .unwrap();
        assert!(start.song.is_none());
        let edited = scheduler
            .check(&entries, before, Check::Edited, |key| key != "missing")
            .unwrap();
        assert_eq!(edited.song.unwrap().key, "first");
        let due = time(19, 9, 8, 0);
        let update = scheduler
            .check(
                &entries,
                due,
                Check::Tick {
                    previous: Some(due - Duration::seconds(1)),
                },
                |key| key != "missing",
            )
            .unwrap();
        assert_eq!(update.song.unwrap().key, "first");
    }

    #[test]
    fn ended_programs_are_excluded_at_their_end_including_catchup() {
        let entries = [
            program("ending", "09:00", "10:00", &[("expired", "09:59")]),
            program("following", "10:00", "11:00", &[("future", "10:01")]),
        ];
        let mut scheduler = Scheduler::default();
        let now = time(19, 10, 0, 0);
        let update = scheduler
            .check(
                &entries,
                now,
                Check::Tick {
                    previous: Some(now - Duration::seconds(30)),
                },
                |_| true,
            )
            .unwrap();
        assert_eq!(update.entry.id, "following");
        assert!(update.song.is_none());
        assert!(
            scheduler
                .check(
                    &entries,
                    time(19, 11, 0, 0),
                    Check::Tick {
                        previous: Some(now)
                    },
                    |_| true
                )
                .is_none()
        );
    }

    #[test]
    fn next_day_reactivates_the_same_program() {
        let entries = [program("daily", "09:00", "10:00", &[("opening", "09:00")])];
        let mut scheduler = Scheduler::default();
        let before = time(19, 9, 0, 0);
        let after = time(20, 9, 0, 0);
        scheduler
            .check(&entries, before, Check::Tick { previous: None }, |_| true)
            .unwrap();
        let update = scheduler
            .check(
                &entries,
                after,
                Check::Tick {
                    previous: Some(before),
                },
                |_| true,
            )
            .unwrap();
        assert!(update.switched);
        assert_eq!(update.song.unwrap().key, "opening");
    }

    #[test]
    fn exact_catchup_and_wake_limits() {
        let entries = [program("morning", "09:00", "10:00", &[("due", "09:05")])];
        let before = time(19, 9, 4, 59);
        let cue = time(19, 9, 5, 0);
        for (age, gap, expected) in [(120, 90, true), (121, 90, false), (121, 91, true)] {
            let mut scheduler = Scheduler::default();
            scheduler.check(&entries, before, Check::Tick { previous: None }, |_| true);
            let now = cue + Duration::seconds(age);
            let update = scheduler.check(
                &entries,
                now,
                Check::Tick {
                    previous: Some(now - Duration::seconds(gap)),
                },
                |_| true,
            );
            assert_eq!(update.is_some(), expected, "age {age}, gap {gap}");
        }
    }
}
