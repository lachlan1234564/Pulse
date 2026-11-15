//! Song lyrics from LRCLIB (lrclib.net): a free, open lyrics database made for music
//! players. No key needed. Many songs have time-synced lyrics (each line with the
//! moment it's sung), so the current line can be highlighted; others have plain text.
//!
//! When LRCLIB has nothing, AZLyrics is tried as a fallback (plain text, no timings).
//!
//! Lyrics are only asked for when you open them, and every answer (including "none
//! found") is kept on disk, so a song's lyrics are fetched once.

use crate::deezer::Track;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const API: &str = "https://lrclib.net/api";
/// "Not found" is asked again after a week (LRCLIB keeps adding songs).
const RETRY_MISSING_AFTER: u64 = 7 * 24 * 3600;
/// Which lookups a saved answer went through. A "not found" saved with a lower number
/// is looked up again straight away.
/// - none/0: LRCLIB only (1.3.0 and older)
/// - 2: LRCLIB, then Genius (the 12 Nov Genius builds; Genius was later removed)
/// - 3: LRCLIB, then AZLyrics
const CACHE_VERSION: u32 = 3;

/// TEMPORARY: a step-by-step log of each lyrics lookup, to find why the AZLyrics
/// fallback doesn't show. Written to `<data folder>\logs\lyrics-debug.log`.
/// Remove once the fallback is confirmed working.
pub fn debug(msg: &str) {
    use std::io::Write;
    if cfg!(test) {
        return;
    }
    let dir = crate::settings::data_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("lyrics-debug.log");
    // Kept small: past 512 KB the log starts over.
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 512 * 1024) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "[{}] {msg}", now());
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Lyrics {
    /// (seconds into the song, line), in order. Empty when only plain lyrics exist.
    pub synced: Vec<(f32, String)>,
    pub plain: Option<String>,
    pub instrumental: bool,
}

impl Lyrics {
    pub fn is_empty(&self) -> bool {
        self.synced.is_empty() && self.plain.as_deref().is_none_or(|p| p.trim().is_empty()) && !self.instrumental
    }

    /// The line being sung at `secs` (its index), if the lyrics are synced.
    pub fn line_at(&self, secs: f32) -> Option<usize> {
        let i = self.synced.partition_point(|(t, _)| *t <= secs);
        i.checked_sub(1)
    }

    /// When the next line starts after `secs`, for waking up exactly then.
    pub fn next_change(&self, secs: f32) -> Option<f32> {
        self.synced.iter().map(|(t, _)| *t).find(|t| *t > secs)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    #[serde(default)]
    synced_lyrics: Option<String>,
    #[serde(default)]
    plain_lyrics: Option<String>,
    #[serde(default)]
    instrumental: bool,
    #[serde(default)]
    duration: f32,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    /// When it was looked up (seconds since 1970).
    at: u64,
    lyrics: Option<Lyrics>,
    /// True if the lyrics came from the fallback (AZLyrics): LRCLIB is asked again
    /// after a week, in case it has added synced lyrics since.
    #[serde(default)]
    fallback: bool,
    /// See CACHE_VERSION. Missing (0) in files saved before the fallback existed.
    #[serde(default)]
    v: u32,
}

/// What to remember about one lookup.
#[derive(Debug, PartialEq)]
enum Keep {
    /// Save it (`fallback`: it came from AZLyrics).
    Save { fallback: bool },
    /// Don't save: a service was down, so the answer may be different next time.
    Skip,
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn dir() -> PathBuf {
    crate::settings::data_dir().join("cache").join("lyrics")
}

fn file_for(t: &Track) -> PathBuf {
    let key = format!("{}|{}|{}", t.id, t.title, t.artist.name);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    dir().join(format!("{h:016x}.json"))
}

/// The song's lyrics: from the disk copy if there is one, else from LRCLIB, else
/// from AZLyrics. `Ok(None)` means neither has them.
pub fn get(t: &Track) -> Result<Option<Lyrics>, String> {
    debug(&format!("--- lookup: \"{}\" by \"{}\" (id {}, {} s)", t.title, t.artist.name, t.id, t.duration));
    lookup(&file_for(t), now(), || fetch(t), || crate::azlyrics::fetch(t))
}

/// One lookup: the saved answer if it's still good, else LRCLIB, then AZLyrics if
/// LRCLIB has nothing usable. The two providers are passed in so tests can stand in for them.
fn lookup(
    path: &std::path::Path,
    now: u64,
    lrclib: impl FnOnce() -> Result<Option<Lyrics>, String>,
    azlyrics: impl FnOnce() -> Result<Option<String>, String>,
) -> Result<Option<Lyrics>, String> {
    if let Some(saved) = load(path, now) {
        return Ok(saved);
    }
    debug("LRCLIB: lookup");
    let primary = lrclib();
    debug(&format!(
        "LRCLIB: {}",
        match &primary {
            Ok(l) => describe(l),
            Err(e) => format!("error: {e}"),
        }
    ));
    let found = combine(primary, || {
        debug("AZLyrics: fallback attempt");
        let r = azlyrics();
        debug(&match &r {
            Ok(Some(p)) => format!("AZLyrics: fallback succeeded, {} lines", p.lines().count()),
            Ok(None) => "AZLyrics: fallback failed, no confident match".into(),
            Err(e) => format!("AZLyrics: fallback failed, error: {e}"),
        });
        r.map(|p| p.filter(|p| !p.trim().is_empty()).map(plain))
    });
    debug(&format!(
        "result: {} ({})",
        match &found {
            Ok((l, _)) => describe(l),
            Err(e) => format!("error: {e}"),
        },
        match &found {
            Ok((_, Keep::Save { fallback: true })) => "saved, from AZLyrics",
            Ok((_, Keep::Save { .. })) => "saved",
            _ => "not saved, will retry next time",
        }
    ));
    let (found, keep) = found?;
    if let Keep::Save { fallback } = keep {
        store(path, &found, fallback, now);
    }
    Ok(found)
}

/// LRCLIB's answer, falling back to AZLyrics when LRCLIB has nothing usable (or is
/// down). Synced LRCLIB lyrics are never replaced.
fn combine(primary: Result<Option<Lyrics>, String>, fallback: impl FnOnce() -> Result<Option<Lyrics>, String>) -> Result<(Option<Lyrics>, Keep), String> {
    // A record with no words and not marked instrumental is as good as none.
    let primary = primary.map(|l| l.filter(|l| !l.is_empty()));
    match primary {
        Ok(Some(l)) => Ok((Some(l), Keep::Save { fallback: false })),
        Ok(None) => match fallback() {
            Ok(Some(l)) => Ok((Some(l), Keep::Save { fallback: true })),
            Ok(None) => Ok((None, Keep::Save { fallback: false })),
            // LRCLIB really has none; AZLyrics just couldn't be reached. Ask again next time.
            Err(_) => Ok((None, Keep::Skip)),
        },
        // LRCLIB is down: show AZLyrics' text for now, but don't keep it.
        Err(e) => match fallback() {
            Ok(Some(l)) => Ok((Some(l), Keep::Skip)),
            _ => Err(e),
        },
    }
}

fn describe(l: &Option<Lyrics>) -> String {
    match l {
        None => "none".into(),
        Some(l) if !l.synced.is_empty() => format!("synced, {} lines", l.synced.len()),
        Some(l) if l.plain.is_some() => "plain only".into(),
        Some(l) if l.instrumental => "instrumental".into(),
        Some(_) => "empty".into(),
    }
}

fn plain(text: String) -> Lyrics {
    Lyrics { plain: Some(text), ..Default::default() }
}

/// The saved answer, unless it's due to be asked again (a "none found" or a
/// fallback answer older than a week, or a "none found" from before AZLyrics).
fn load(path: &std::path::Path, now: u64) -> Option<Option<Lyrics>> {
    let text = std::fs::read_to_string(path).ok()?;
    let Ok(saved) = serde_json::from_str::<Saved>(&text) else {
        debug("cache: ignored unreadable entry");
        return None;
    };
    let age = now.saturating_sub(saved.at);
    let fresh = age < RETRY_MISSING_AFTER;
    let (keep, why) = match (&saved.lyrics, saved.fallback) {
        (None, _) if saved.v < CACHE_VERSION => (false, format!("ignored legacy negative entry (version {} < {CACHE_VERSION}, AZLyrics never asked)", saved.v)),
        (None, _) if !fresh => (false, "expired negative entry (over a week old)".into()),
        (None, _) => (true, "negative hit (LRCLIB and AZLyrics had none, under a week ago)".into()),
        (Some(_), true) if !fresh => (false, "expired AZLyrics entry (over a week old, checking LRCLIB for synced lyrics)".into()),
        (Some(l), true) => (true, format!("positive hit from AZLyrics ({})", describe(&Some(l.clone())))),
        (Some(l), false) => (true, format!("positive hit from LRCLIB ({})", describe(&Some(l.clone())))),
    };
    debug(&format!("cache: {why}, saved {} h ago", age / 3600));
    keep.then_some(saved.lyrics)
}

fn store(path: &std::path::Path, lyrics: &Option<Lyrics>, fallback: bool, now: u64) {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(json) = serde_json::to_string(&Saved { at: now, lyrics: lyrics.clone(), fallback, v: CACHE_VERSION }) {
        let _ = std::fs::write(path, json);
    }
}

fn fetch(t: &Track) -> Result<Option<Lyrics>, String> {
    // "Baby (feat. DaBaby)" is listed as "Baby".
    let title = crate::youtube::core_title(&t.title);
    let title = if title.trim().is_empty() { t.title.clone() } else { title };
    let artist = t.artist.name.clone();
    let duration = t.duration.to_string();
    let album = t.album.as_ref().map(|a| a.title.clone()).unwrap_or_default();

    // 1. Exact match: title, artist, album and length (LRCLIB allows ±2 seconds).
    let mut q: Vec<(&str, &str)> = vec![("track_name", &title), ("artist_name", &artist)];
    if !album.is_empty() {
        q.push(("album_name", &album));
    }
    if t.duration > 0 {
        q.push(("duration", &duration));
    }
    match request::<Record>("/get", &q) {
        Ok(Some(r)) => return Ok(Some(from_record(r))),
        Ok(None) => {}
        Err(e) => return Err(e),
    }
    // 2. Search by title and artist; take the closest length.
    let list = request::<Vec<Record>>("/search", &[("track_name", &title), ("artist_name", &artist)])?.unwrap_or_default();
    let best = list
        .into_iter()
        .filter(|r| r.synced_lyrics.is_some() || r.plain_lyrics.is_some() || r.instrumental)
        .filter(|r| t.duration == 0 || r.duration == 0.0 || (r.duration - t.duration as f32).abs() <= 8.0)
        .min_by(|a, b| {
            let d = |r: &Record| (r.duration - t.duration as f32).abs() - if r.synced_lyrics.is_some() { 3.0 } else { 0.0 };
            d(a).total_cmp(&d(b))
        });
    Ok(best.map(from_record))
}

/// One LRCLIB request; `Ok(None)` when it has nothing.
fn request<T: for<'de> Deserialize<'de>>(path: &str, query: &[(&str, &str)]) -> Result<Option<T>, String> {
    let mut req = crate::net::api().get(format!("{API}{path}"));
    for (k, v) in query {
        req = req.query(*k, *v);
    }
    match req.call() {
        Ok(mut resp) => resp.body_mut().read_json::<T>().map(Some).map_err(|e| format!("Couldn't read the lyrics ({e})")),
        Err(ureq::Error::StatusCode(404)) => Ok(None),
        Err(e) => Err(crate::net::friendly_error(e)),
    }
}

fn from_record(r: Record) -> Lyrics {
    Lyrics { synced: r.synced_lyrics.as_deref().map(parse_lrc).unwrap_or_default(), plain: r.plain_lyrics.filter(|p| !p.trim().is_empty()), instrumental: r.instrumental }
}

/// Reads LRC text: "[01:02.50]Line" (a line can have several times: "[00:10][01:10]Chorus").
pub fn parse_lrc(text: &str) -> Vec<(f32, String)> {
    let mut out = vec![];
    for raw in text.lines() {
        let mut rest = raw.trim();
        let mut times = vec![];
        while let Some(stripped) = rest.strip_prefix('[') {
            let Some(end) = stripped.find(']') else { break };
            let tag = &stripped[..end];
            match parse_time(tag) {
                Some(t) => times.push(t),
                // A tag like [ar:Artist]: not a lyric line.
                None => {
                    times.clear();
                    rest = "";
                    break;
                }
            }
            rest = &stripped[end + 1..];
        }
        let line = rest.trim().to_string();
        for t in times {
            out.push((t, line.clone()));
        }
    }
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out
}

fn parse_time(tag: &str) -> Option<f32> {
    let (m, s) = tag.split_once(':')?;
    let m: f32 = m.trim().parse().ok()?;
    let s: f32 = s.trim().parse().ok()?;
    (m >= 0.0 && (0.0..60.0).contains(&s)).then_some(m * 60.0 + s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lrc_lines_and_repeated_times_are_read() {
        let l = parse_lrc("[ar:Someone]\n[00:01.50]First\n[00:10.00][00:30.00]Chorus\n[00:20.25]\n[bad]text");
        assert_eq!(l, vec![(1.5, "First".into()), (10.0, "Chorus".into()), (20.25, String::new()), (30.0, "Chorus".into())]);
    }

    fn synced() -> Lyrics {
        Lyrics { synced: parse_lrc("[00:01]a"), ..Default::default() }
    }

    #[test]
    fn lrclib_lyrics_are_kept_and_the_fallback_is_not_asked() {
        let r = combine(Ok(Some(synced())), || panic!("AZLyrics shouldn't be asked")).unwrap();
        assert_eq!(r, (Some(synced()), Keep::Save { fallback: false }));
    }

    #[test]
    fn the_fallback_is_used_when_lrclib_has_none() {
        let r = combine(Ok(None), || Ok(Some(plain("words".into())))).unwrap();
        assert_eq!(r, (Some(plain("words".into())), Keep::Save { fallback: true }));
        assert_eq!(combine(Ok(None), || Ok(None)).unwrap(), (None, Keep::Save { fallback: false }));
        // A fallback that couldn't connect isn't remembered as "none".
        assert_eq!(combine(Ok(None), || Err("down".into())).unwrap(), (None, Keep::Skip));
    }

    #[test]
    fn lrclib_down_uses_the_fallback_without_saving_it() {
        assert_eq!(combine(Err("down".into()), || Ok(Some(plain("w".into())))).unwrap(), (Some(plain("w".into())), Keep::Skip));
        assert_eq!(combine(Err("down".into()), || Ok(None)), Err("down".into()));
    }

    #[test]
    fn fallback_answers_are_cached_then_rechecked_after_a_week() {
        let dir = std::env::temp_dir().join(format!("pulse-lyrics-test-{}", std::process::id()));
        let (az, lrc) = (dir.join("az.json"), dir.join("lrc.json"));
        store(&az, &Some(plain("words".into())), true, now());
        store(&lrc, &Some(synced()), false, now());
        let later = now() + RETRY_MISSING_AFTER + 1;
        assert_eq!(load(&az, now()), Some(Some(plain("words".into()))));
        assert_eq!(load(&az, later), None);
        assert_eq!(load(&lrc, later), Some(Some(synced())));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn old_not_found_answers_are_looked_up_again() {
        let dir = std::env::temp_dir().join(format!("pulse-lyrics-old-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let (old_miss, old_hit) = (dir.join("miss.json"), dir.join("hit.json"));
        // Saved by Pulse 1.3.0: no "v", no "fallback".
        std::fs::write(&old_miss, format!(r#"{{"at":{},"lyrics":null}}"#, now())).unwrap();
        std::fs::write(&old_hit, format!(r#"{{"at":{},"lyrics":{{"synced":[[1.0,"a"]],"plain":null,"instrumental":false}}}}"#, now())).unwrap();
        assert_eq!(load(&old_miss, now()), None);
        assert_eq!(load(&old_hit, now()), Some(Some(synced())));
        // A new "not found" (both providers asked) is kept for a week.
        store(&old_miss, &None, false, now());
        assert_eq!(load(&old_miss, now()), Some(None));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The exact file the 12 Nov Genius build left for "The Lady In Red": version 2,
    /// meaning LRCLIB and Genius were asked, never AZLyrics. It must be looked up again.
    #[test]
    fn a_not_found_from_the_genius_build_is_looked_up_again() {
        let dir = std::env::temp_dir().join(format!("pulse-lyrics-genius-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("aaa10478294096de.json");
        std::fs::write(&f, format!(r#"{{"at":{},"v":2,"lyrics":null}}"#, now() - 2 * 24 * 3600)).unwrap();
        assert_eq!(load(&f, now()), None);
        // Once AZLyrics has been asked too, a "not found" is trusted for a week.
        store(&f, &None, false, now());
        assert_eq!(load(&f, now()), Some(None));
        // And AZLyrics' lyrics, once found, are used.
        store(&f, &Some(plain("words".into())), true, now());
        assert_eq!(load(&f, now()), Some(Some(plain("words".into()))));
        let _ = std::fs::remove_dir_all(dir);
    }

    // ---- Whole lookups, with stand-ins for LRCLIB and AZLyrics (no network). ----

    use std::cell::RefCell;

    fn temp_file(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pulse-lyrics-lookup-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("song.json")
    }

    /// Runs one lookup and returns its answer plus which providers were asked, in order.
    fn run(path: &std::path::Path, at: u64, lrclib: Result<Option<Lyrics>, String>, az: Result<Option<String>, String>) -> (Result<Option<Lyrics>, String>, Vec<&'static str>) {
        let calls = RefCell::new(vec![]);
        let r = lookup(
            path,
            at,
            || {
                calls.borrow_mut().push("lrclib");
                lrclib
            },
            || {
                calls.borrow_mut().push("azlyrics");
                az
            },
        );
        (r, calls.into_inner())
    }

    /// A lookup that must be answered from the cache: any provider call fails the test.
    fn cached_only(path: &std::path::Path, at: u64) -> Option<Lyrics> {
        lookup(path, at, || panic!("LRCLIB asked despite a good saved answer"), || panic!("AZLyrics asked despite a good saved answer")).unwrap()
    }

    /// The original bug: an old "not found" (from the LRCLIB-only or Genius builds)
    /// returned early, so AZLyrics never ran. It must reach LRCLIB, then AZLyrics,
    /// and keep what AZLyrics found.
    #[test]
    fn regression_old_cached_none_does_not_block_the_fallback() {
        for (name, old) in [("genius", r#"{"at":AT,"v":2,"lyrics":null}"#), ("lrclib-only", r#"{"at":AT,"lyrics":null}"#), ("az-v2", r#"{"at":AT,"lyrics":null,"fallback":false,"v":2}"#)] {
            let f = temp_file(name);
            let t0 = now();
            std::fs::write(&f, old.replace("AT", &(t0 - 3600).to_string())).unwrap();
            let (r, calls) = run(&f, t0, Ok(None), Ok(Some("found words\nsecond line".into())));
            assert_eq!(calls, vec!["lrclib", "azlyrics"], "{name}: both providers, LRCLIB first");
            assert_eq!(r.unwrap(), Some(plain("found words\nsecond line".into())), "{name}");
            // Kept: the next lookup is answered from the cache.
            assert_eq!(cached_only(&f, t0 + 60), Some(plain("found words\nsecond line".into())), "{name}");
        }
    }

    #[test]
    fn a_new_cached_none_is_trusted_for_a_week_then_rechecked() {
        let f = temp_file("negative");
        let t0 = now();
        let (r, calls) = run(&f, t0, Ok(None), Ok(None));
        assert_eq!((r.unwrap(), calls), (None, vec!["lrclib", "azlyrics"]));
        assert_eq!(cached_only(&f, t0 + 3600), None, "negative hit: no requests");
        let (_, calls) = run(&f, t0 + RETRY_MISSING_AFTER + 1, Ok(None), Ok(None));
        assert_eq!(calls, vec!["lrclib", "azlyrics"], "expired: asked again");
    }

    #[test]
    fn synced_lrclib_lyrics_win_and_are_cached() {
        let f = temp_file("synced");
        let t0 = now();
        let (r, calls) = run(&f, t0, Ok(Some(synced())), Ok(Some("plain words".into())));
        assert_eq!((r.unwrap(), calls), (Some(synced()), vec!["lrclib"]));
        assert_eq!(cached_only(&f, t0 + 2 * RETRY_MISSING_AFTER), Some(synced()), "LRCLIB answers don't expire");
    }

    #[test]
    fn an_empty_lrclib_record_still_falls_back() {
        let f = temp_file("empty");
        let (r, calls) = run(&f, now(), Ok(Some(Lyrics::default())), Ok(Some("words".into())));
        assert_eq!((r.unwrap(), calls), (Some(plain("words".into())), vec!["lrclib", "azlyrics"]));
    }

    #[test]
    fn a_failed_fallback_is_not_cached() {
        let f = temp_file("az-down");
        let t0 = now();
        let (r, _) = run(&f, t0, Ok(None), Err("AZLyrics unreachable".into()));
        assert_eq!(r.unwrap(), None);
        assert!(!f.exists(), "a network failure must not be saved as \"not found\"");
        let (r, calls) = run(&f, t0 + 60, Ok(None), Ok(Some("words".into())));
        assert_eq!((r.unwrap(), calls), (Some(plain("words".into())), vec!["lrclib", "azlyrics"]));
    }

    #[test]
    fn lrclib_down_is_not_cached() {
        let f = temp_file("lrclib-down");
        let (r, calls) = run(&f, now(), Err("LRCLIB down".into()), Ok(None));
        assert_eq!((r, calls), (Err("LRCLIB down".into()), vec!["lrclib", "azlyrics"]));
        assert!(!f.exists());
        let (r, _) = run(&f, now(), Err("LRCLIB down".into()), Ok(Some("words".into())));
        assert_eq!(r.unwrap(), Some(plain("words".into())), "AZLyrics' words are shown");
        assert!(!f.exists(), "but not kept, so LRCLIB is asked when it's back");
    }

    #[test]
    fn an_expired_azlyrics_answer_gives_lrclib_another_chance() {
        let f = temp_file("az-expired");
        let t0 = now();
        run(&f, t0, Ok(None), Ok(Some("words".into()))).0.unwrap();
        let (r, calls) = run(&f, t0 + RETRY_MISSING_AFTER + 1, Ok(Some(synced())), Ok(None));
        assert_eq!((r.unwrap(), calls), (Some(synced()), vec!["lrclib"]));
        assert_eq!(cached_only(&f, t0 + 3 * RETRY_MISSING_AFTER), Some(synced()));
    }

    #[test]
    fn the_current_line_follows_the_time() {
        let lyr = Lyrics { synced: parse_lrc("[00:05]a\n[00:10]b\n[00:15]c"), ..Default::default() };
        assert_eq!(lyr.line_at(2.0), None);
        assert_eq!(lyr.line_at(5.0), Some(0));
        assert_eq!(lyr.line_at(12.0), Some(1));
        assert_eq!(lyr.line_at(99.0), Some(2));
        assert_eq!(lyr.next_change(12.0), Some(15.0));
        assert_eq!(lyr.next_change(20.0), None);
    }
}
