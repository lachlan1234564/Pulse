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
/// Which lookups a saved answer went through. 2 = LRCLIB, then AZLyrics. A "not found"
/// saved by an older Pulse (LRCLIB only) is looked up again straight away.
const CACHE_VERSION: u32 = 2;

/// TEMPORARY: a step-by-step log of each lyrics lookup, to find why the AZLyrics
/// fallback doesn't show. Written to `<data folder>\logs\lyrics-debug.log`.
/// Remove once the fallback is confirmed working.
pub fn debug(msg: &str) {
    use std::io::Write;
    let dir = crate::settings::data_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("lyrics-debug.log")) {
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
    let path = file_for(t);
    debug(&format!("--- lookup: \"{}\" by \"{}\" (id {}, {} s)", t.title, t.artist.name, t.id, t.duration));
    if let Some(saved) = load(&path, now()) {
        debug(&format!("cache: using saved answer ({})", describe(&saved)));
        return Ok(saved);
    }
    let primary = fetch(t);
    debug(&format!(
        "LRCLIB: {}",
        match &primary {
            Ok(l) => describe(l),
            Err(e) => format!("error: {e}"),
        }
    ));
    let found = combine(primary, || {
        debug("AZLyrics fallback: triggered");
        let r = crate::azlyrics::fetch(t);
        debug(&format!(
            "AZLyrics: {}",
            match &r {
                Ok(Some(p)) => format!("found {} lines", p.lines().count()),
                Ok(None) => "no confident match".into(),
                Err(e) => format!("error: {e}"),
            }
        ));
        r.map(|p| p.map(plain))
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
            _ => "not saved",
        }
    ));
    let (found, keep) = found?;
    if let Keep::Save { fallback } = keep {
        store(&path, &found, fallback);
    }
    Ok(found)
}

/// LRCLIB's answer, falling back to AZLyrics when LRCLIB has none (or is down).
/// Synced LRCLIB lyrics are never replaced.
fn combine(primary: Result<Option<Lyrics>, String>, fallback: impl FnOnce() -> Result<Option<Lyrics>, String>) -> Result<(Option<Lyrics>, Keep), String> {
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
/// fallback answer older than a week).
fn load(path: &std::path::Path, now: u64) -> Option<Option<Lyrics>> {
    let saved: Saved = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    // A "not found" from before AZLyrics was added never asked AZLyrics: look again.
    if saved.lyrics.is_none() && saved.v < CACHE_VERSION {
        return None;
    }
    let fresh = now.saturating_sub(saved.at) < RETRY_MISSING_AFTER;
    (fresh || (saved.lyrics.is_some() && !saved.fallback)).then_some(saved.lyrics)
}

fn store(path: &std::path::Path, lyrics: &Option<Lyrics>, fallback: bool) {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(json) = serde_json::to_string(&Saved { at: now(), lyrics: lyrics.clone(), fallback, v: CACHE_VERSION }) {
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
        store(&az, &Some(plain("words".into())), true);
        store(&lrc, &Some(synced()), false);
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
        store(&old_miss, &None, false);
        assert_eq!(load(&old_miss, now()), Some(None));
        let _ = std::fs::remove_dir_all(dir);
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
