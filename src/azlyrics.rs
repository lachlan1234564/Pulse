//! Fallback lyrics from AZLyrics (azlyrics.com), used only when LRCLIB has none.
//!
//! AZLyrics has no public API, so this reads the song's page. It gives plain text
//! only (no timings). The page address is built from the artist and title, and the
//! page's own artist and title are checked against the song before its lyrics are
//! used, so a near miss returns nothing rather than the wrong song's words.
//!
//! Written for Pulse from scratch. Requests use Pulse's normal user agent.

use crate::deezer::Track;

const SITE: &str = "https://www.azlyrics.com/lyrics";
/// How close (0–1) the page's title and artist must be to the song's.
const MIN_MATCH: f64 = 0.9;

/// The song's lyrics from AZLyrics. `Ok(None)` when there's no confident match.
/// `Err` only for network trouble, so a temporary failure isn't remembered as "none".
pub fn fetch(t: &Track) -> Result<Option<String>, String> {
    let artist = t.artist.name.trim();
    if artist.is_empty() || t.title.trim().is_empty() {
        return Ok(None);
    }
    let mut tried = vec![];
    for title in title_variants(&t.title) {
        let Some(url) = page_url(artist, &title) else {
            continue;
        };
        if tried.contains(&url) {
            continue;
        }
        tried.push(url.clone());
        crate::lyrics::debug(&format!("AZLyrics: GET {url}"));
        let html = match crate::net::api().get(&url).call() {
            Ok(mut resp) => {
                let status = resp.status();
                let body = resp.body_mut().read_to_string().map_err(|e| format!("Couldn't read the lyrics page ({e})"));
                crate::lyrics::debug(&format!(
                    "AZLyrics:   HTTP {status}, {} bytes, page title: {}",
                    body.as_ref().map(|b| b.len()).unwrap_or(0),
                    body.as_deref().ok().and_then(title_tag).unwrap_or("(none)".into())
                ));
                body?
            }
            Err(ureq::Error::StatusCode(404)) => {
                crate::lyrics::debug("AZLyrics:   HTTP 404 (no page at this address)");
                continue;
            }
            Err(e) => {
                crate::lyrics::debug(&format!("AZLyrics:   request failed: {e}"));
                return Err(crate::net::friendly_error(e));
            }
        };
        let names = page_names(&html);
        crate::lyrics::debug(&format!("AZLyrics:   page says: {names:?}"));
        match lyrics_if_match(&html, artist, &title) {
            Some(lyrics) => {
                crate::lyrics::debug(&format!("AZLyrics:   matched {url}, {} lines extracted", lyrics.lines().count()));
                return Ok(Some(lyrics));
            }
            None => crate::lyrics::debug(&format!(
                "AZLyrics:   rejected (names match: {}, lyrics found: {})",
                names.as_ref().is_some_and(|(a, t)| names_match(a, t, artist, &title)),
                extract(&html).is_some()
            )),
        }
    }
    Ok(None)
}

/// "Baby (feat. DaBaby)" → "Baby (feat. DaBaby)", "Baby"; "Song - Remastered 2011" → also "Song".
fn title_variants(title: &str) -> Vec<String> {
    let mut out = vec![];
    let core = crate::youtube::core_title(title);
    for t in [core.as_str(), title] {
        let t = t.trim();
        if !t.is_empty() && !out.iter().any(|o: &String| o == t) {
            out.push(t.to_string());
        }
    }
    // Drop a trailing "(…)", "[…]" or " - …" (versions, remasters) as a last try.
    let base = cut_suffix(&core);
    if !base.is_empty() && !out.contains(&base) {
        out.push(base);
    }
    out
}

fn cut_suffix(t: &str) -> String {
    let i = [" (", " [", " - "].iter().filter_map(|m| t.find(m)).min();
    i.map(|i| t[..i].trim().to_string()).unwrap_or_default()
}

/// Letters and digits only, lower case: how AZLyrics spells names in its addresses.
fn slug(s: &str) -> String {
    let s = s.replace('&', "and");
    s.chars()
        .flat_map(fold)
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// "é" → "e", so accented names still make a usable address.
fn fold(c: char) -> Vec<char> {
    let base = match c {
        'à'..='å' | 'À'..='Å' => 'a',
        'è'..='ë' | 'È'..='Ë' => 'e',
        'ì'..='ï' | 'Ì'..='Ï' => 'i',
        'ò'..='ö' | 'ø' | 'Ò'..='Ö' | 'Ø' => 'o',
        'ù'..='ü' | 'Ù'..='Ü' => 'u',
        'ñ' | 'Ñ' => 'n',
        'ç' | 'Ç' => 'c',
        'ý' | 'ÿ' | 'Ý' => 'y',
        'ß' => return vec!['s', 's'],
        other => other,
    };
    vec![base]
}

/// AZLyrics files "The Weeknd" under "weeknd".
fn artist_slug(artist: &str) -> String {
    let a = artist.trim();
    let a = if a.len() > 4 && a[..4].eq_ignore_ascii_case("the ") {
        &a[4..]
    } else {
        a
    };
    slug(a)
}

fn page_url(artist: &str, title: &str) -> Option<String> {
    let (a, t) = (artist_slug(artist), slug(title));
    (!a.is_empty() && !t.is_empty()).then(|| format!("{SITE}/{a}/{t}.html"))
}

/// The lyrics on `html`, but only if the page is for this artist and title.
fn lyrics_if_match(html: &str, artist: &str, title: &str) -> Option<String> {
    let (page_artist, page_title) = page_names(html)?;
    if !names_match(&page_artist, &page_title, artist, title) {
        return None;
    }
    extract(html)
}

fn names_match(page_artist: &str, page_title: &str, artist: &str, title: &str) -> bool {
    similarity(&artist_slug(page_artist), &artist_slug(artist)) >= MIN_MATCH && similarity(&slug(page_title), &slug(title)) >= MIN_MATCH
}

/// The page's <title>, for the debug log (a block page has a different one).
fn title_tag(html: &str) -> Option<String> {
    let i = html.find("<title>")? + 7;
    let e = html[i..].find("</title>")?;
    Some(decode(html[i..i + e].trim()))
}

/// The artist and title the page says it is for: `<b>Artist Lyrics</b>` then `<b>"Title"</b>`.
fn page_names(html: &str) -> Option<(String, String)> {
    let mut bolds = html.match_indices("<b>").filter_map(|(i, _)| {
        let rest = &html[i + 3..];
        rest.find("</b>")
            .map(|e| decode(&strip_tags(&rest[..e])).trim().to_string())
    });
    let artist = bolds.find(|b| b.ends_with(" Lyrics"))?;
    let artist = artist.trim_end_matches(" Lyrics").trim().to_string();
    let title = bolds.find(|b| b.len() > 2 && b.starts_with('"') && b.ends_with('"'))?;
    Some((artist, title.trim_matches('"').trim().to_string()))
}

/// The lyrics block: the plain `<div>` (no class or id) with the most text.
fn extract(html: &str) -> Option<String> {
    let mut best: Option<String> = None;
    for (i, _) in html.match_indices("<div>") {
        let body = &html[i + 5..];
        let Some(end) = body.find("</div>") else {
            continue;
        };
        let text = clean(&body[..end]);
        if best.as_ref().is_none_or(|b| text.len() > b.len()) {
            best = Some(text);
        }
    }
    // Too short to be a song's words (an empty block or a one-line notice).
    best.filter(|t| t.lines().filter(|l| !l.trim().is_empty()).count() >= 4 && t.len() >= 60)
}

/// HTML → text: <br> to new lines, other tags and comments removed, entities decoded,
/// spaces trimmed, and at most one empty line in a row.
fn clean(fragment: &str) -> String {
    let mut s = String::with_capacity(fragment.len());
    let mut rest = fragment;
    while let Some(i) = rest.find('<') {
        s.push_str(&rest[..i].replace(['\r', '\n'], ""));
        let tag_end = if rest[i..].starts_with("<!--") {
            rest[i..].find("-->").map(|e| e + 3)
        } else {
            rest[i..].find('>').map(|e| e + 1)
        };
        let Some(e) = tag_end else {
            rest = "";
            break;
        };
        let tag = rest[i..i + e].to_ascii_lowercase();
        if tag.starts_with("<br") {
            s.push('\n');
        }
        rest = &rest[i + e..];
    }
    s.push_str(&rest.replace(['\r', '\n'], ""));
    let text = decode(&s);
    let mut out: Vec<&str> = vec![];
    for line in text.lines().map(str::trim) {
        if line.is_empty() && out.last().is_none_or(|l| l.is_empty()) {
            continue;
        }
        out.push(line);
    }
    while out.last() == Some(&"") {
        out.pop();
    }
    out.join("\n")
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in s.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            c if !inside => out.push(c),
            _ => {}
        }
    }
    out
}

/// Decodes the HTML entities found in lyrics pages: named ones and &#NN; / &#xNN;.
fn decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        let end = after.find(';').filter(|&e| e <= 10);
        let ch = end.and_then(|e| {
            let name = &after[1..e];
            match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                "rsquo" | "lsquo" => Some('\''),
                "rdquo" | "ldquo" => Some('"'),
                "hellip" => Some('…'),
                "mdash" => Some('—'),
                "ndash" => Some('–'),
                n if n.starts_with("#x") || n.starts_with("#X") => u32::from_str_radix(&n[2..], 16)
                    .ok()
                    .and_then(char::from_u32),
                n if n.starts_with('#') => n[1..].parse().ok().and_then(char::from_u32),
                _ => None,
            }
        });
        match (ch, end) {
            (Some(c), Some(e)) => {
                out.push(c);
                rest = &after[e + 1..];
            }
            _ => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Jaro-Winkler similarity of two strings, 0 (nothing alike) to 1 (the same).
fn similarity(a: &str, b: &str) -> f64 {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.is_empty() || b.is_empty() {
        return if a == b { 1.0 } else { 0.0 };
    }
    let window = (a.len().max(b.len()) / 2).saturating_sub(1);
    let (mut am, mut bm) = (vec![false; a.len()], vec![false; b.len()]);
    let mut matches = 0.0;
    for i in 0..a.len() {
        let (lo, hi) = (i.saturating_sub(window), (i + window + 1).min(b.len()));
        for j in lo..hi {
            if !bm[j] && a[i] == b[j] {
                am[i] = true;
                bm[j] = true;
                matches += 1.0;
                break;
            }
        }
    }
    if matches == 0.0 {
        return 0.0;
    }
    let (ma, mb): (Vec<char>, Vec<char>) = (
        a.iter()
            .zip(&am)
            .filter(|(_, m)| **m)
            .map(|(c, _)| *c)
            .collect(),
        b.iter()
            .zip(&bm)
            .filter(|(_, m)| **m)
            .map(|(c, _)| *c)
            .collect(),
    );
    let half_swaps = ma.iter().zip(&mb).filter(|(x, y)| x != y).count() as f64 / 2.0;
    let jaro =
        (matches / a.len() as f64 + matches / b.len() as f64 + (matches - half_swaps) / matches)
            / 3.0;
    let prefix = a.iter().zip(&b).take(4).take_while(|(x, y)| x == y).count() as f64;
    jaro + prefix * 0.1 * (1.0 - jaro)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r##"<html><head><title>Artist Name - Song Lyrics</title></head><body>
<div class="ringtone"><a href="#">Ringtone</a></div>
<div class="lyricsh"><h2><b>Sigur &amp; Rós Lyrics</b></h2></div>
<b>"Don&#39;t Stop (Me Now)"</b>
<div class="col-xs-12">menu</div>
<div>
<!-- Usage of azlyrics.com content by any third-party lyrics provider is prohibited by our licensing agreement. Sorry about that. -->
First line &amp; more<br>
  Second &quot;line&quot;<br>
<br>
<br>
<br>
<i>[Chorus]</i><br>
Third line it&#x27;s here<br>
Fourth line&nbsp;ends<br>
</div>
<div>short</div>
</body></html>"##;

    #[test]
    fn lyrics_are_extracted_cleaned_and_decoded() {
        let l = extract(PAGE).unwrap();
        assert_eq!(
            l,
            "First line & more\nSecond \"line\"\n\n[Chorus]\nThird line it's here\nFourth line ends"
        );
    }

    /// The layout of a real AZLyrics song page (Nov 2026), with the words replaced.
    const REAL: &str = "<div class=\"div-share\"><h1>\"The Lady In Red\" lyrics</h1></div>\r\n\r\n<div class=\"lyricsh\">\r\n<h2><a href=\"//www.azlyrics.com/19/2hollis.html\"><b>2hollis Lyrics</b></a></h2>\r\n</div>\r\n <div class=\"ringtone\"> <span id=\"cf_text_top\"></span> </div> <b>\"The Lady In Red\"</b><br> <br> <div> <!-- Usage of azlyrics.com content by any third-party lyrics provider is prohibited by our licensing agreement. Sorry about that. -->\r\nLine 1 &amp; it&#039;s<br>\nLine 2<br>\n<br>\nLine 3<br>\nLine 4 <i>(yeah)</i><br>\nLine five is a longer one<br>\nLine six is a longer one too<br>\n</div>\r\n\r\n<br>\r\n<br>\r\n\r\n<!-- MxM banner -->\r\n<div class=\"noprint\" id=\"azmxmbanner\" style=\"display:none;\">\r\n<script>\r\nif (x) {}\r\n</script>\r\n</div>\r\n<b>\"Pirouette\"</b><div></div>";

    #[test]
    fn a_real_page_layout_is_read() {
        assert_eq!(page_names(REAL), Some(("2hollis".into(), "The Lady In Red".into())));
        let l = lyrics_if_match(REAL, "2hollis", "the lady in red").unwrap();
        assert_eq!(l, "Line 1 & it's\nLine 2\n\nLine 3\nLine 4 (yeah)\nLine five is a longer one\nLine six is a longer one too");
        assert_eq!(title_tag("<title>2hollis - The Lady In Red Lyrics | AZLyrics.com</title>").as_deref(), Some("2hollis - The Lady In Red Lyrics | AZLyrics.com"));
    }

    #[test]
    fn entities_decode() {
        assert_eq!(
            decode("A&amp;B &lt;x&gt; &#233; &#xE9; &rsquo; &bogus; & end"),
            "A&B <x> é é ' &bogus; & end"
        );
    }

    #[test]
    fn empty_or_tiny_lyrics_are_rejected() {
        assert_eq!(extract("<div>\n<br>\n</div>"), None);
        assert_eq!(extract("<div>We don't have these lyrics yet.</div>"), None);
        assert_eq!(extract("<p>no divs here</p>"), None);
    }

    #[test]
    fn page_names_are_read() {
        assert_eq!(
            page_names(PAGE),
            Some(("Sigur & Rós".into(), "Don't Stop (Me Now)".into()))
        );
    }

    #[test]
    fn the_wrong_song_is_rejected() {
        assert!(lyrics_if_match(PAGE, "Sigur & Rós", "Don't Stop (Me Now)").is_some());
        assert!(lyrics_if_match(PAGE, "Sigur and Ros", "Dont Stop Me Now").is_some());
        assert!(lyrics_if_match(PAGE, "Sigur & Rós", "Don't Stop Believin'").is_none());
        assert!(lyrics_if_match(PAGE, "Queen", "Don't Stop (Me Now)").is_none());
        assert!(lyrics_if_match("<div>no names</div>", "Queen", "Song").is_none());
    }

    #[test]
    fn addresses_handle_punctuation_and_special_characters() {
        assert_eq!(
            page_url("The Weeknd", "Can't Feel My Face").as_deref(),
            Some("https://www.azlyrics.com/lyrics/weeknd/cantfeelmyface.html")
        );
        assert_eq!(
            page_url("Beyoncé", "Déjà Vu").as_deref(),
            Some("https://www.azlyrics.com/lyrics/beyonce/dejavu.html")
        );
        assert_eq!(
            page_url("AC/DC", "T.N.T.").as_deref(),
            Some("https://www.azlyrics.com/lyrics/acdc/tnt.html")
        );
        assert_eq!(
            page_url("2hollis", "the lady in red").as_deref(),
            Some("https://www.azlyrics.com/lyrics/2hollis/theladyinred.html")
        );
        assert_eq!(page_url("!!!", "???"), None);
    }

    #[test]
    fn title_variants_drop_features_and_versions() {
        assert_eq!(
            title_variants("Baby (feat. DaBaby)"),
            vec!["Baby", "Baby (feat. DaBaby)"]
        );
        assert_eq!(
            title_variants("Song - Remastered 2011"),
            vec!["Song - Remastered 2011", "Song"]
        );
        assert_eq!(title_variants("Plain"), vec!["Plain"]);
    }

    #[test]
    fn similarity_scores() {
        assert_eq!(similarity("abc", "abc"), 1.0);
        assert!(similarity("dontstopmenow", "dontstopmenowz") > 0.95);
        assert!(similarity("dontstopmenow", "dontstopbelievin") < MIN_MATCH);
    }
}
