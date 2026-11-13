//! The one file of a repository that changes and is signed: what its
//! catalog and indexes are, which index each channel follows, how many
//! times it has been published, and when. Everything else is named by its
//! hash, from here.
//!
//! ```text
//! sequence 1042
//! signed 2026-11-13T11:30:00Z
//! catalog sha256:…
//! index 2026.10.0 sha256:…
//! index leading sha256:…
//! channel stable 2026.10
//! channel 2026.09 2026.09.3 ended
//! ```
//!
//! An index is the top node of a tree of one version of each package; a
//! channel names an index or another channel, and `ended` marks one that
//! won't move again. Indexes and channels share one namespace. Lines are in
//! this order, indexes and channels each sorted by name.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::Identity;

/// The index that always holds the newest of each package.
pub const LEADING: &str = "leading";

/// A channel: the index or channel it follows, and whether it has ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Channel {
    pub target: String,
    pub ended: bool,
}

/// A repository's pointer file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pointer {
    /// How many times the repository has been published; it only goes up.
    pub sequence: u64,
    /// When it was signed, as `YYYY-MM-DDTHH:MM:SSZ`.
    pub signed: String,
    /// The catalog's top node, once there is one.
    pub catalog: Option<Identity>,
    pub indexes: BTreeMap<String, Identity>,
    pub channels: BTreeMap<String, Channel>,
}

impl Pointer {
    pub fn render(&self) -> String {
        let mut out = format!("sequence {}\nsigned {}\n", self.sequence, self.signed);
        if let Some(catalog) = &self.catalog {
            out.push_str(&format!("catalog {catalog}\n"));
        }
        for (name, top) in &self.indexes {
            out.push_str(&format!("index {name} {top}\n"));
        }
        for (name, channel) in &self.channels {
            out.push_str(&format!("channel {name} {}", channel.target));
            if channel.ended {
                out.push_str(" ended");
            }
            out.push('\n');
        }
        out
    }

    /// Parse a pointer file, which must be in the form [`Pointer::render`]
    /// writes, its channels each reaching an index.
    pub fn parse(text: &str) -> Result<Self> {
        let mut pointer = Self::default();
        let mut lines = text.lines();
        pointer.sequence = (lines.next().and_then(|l| l.strip_prefix("sequence ")))
            .context("the first line must be the sequence")?
            .parse()
            .context("the sequence must be a number")?;
        pointer.signed = (lines.next().and_then(|l| l.strip_prefix("signed ")))
            .context("the second line must be when it was signed")?
            .to_owned();
        check_time(&pointer.signed)?;
        for line in lines {
            let words: Vec<&str> = line.split(' ').collect();
            match words.as_slice() {
                ["catalog", top] if pointer.catalog.is_none() => {
                    pointer.catalog = Some(top.parse()?);
                }
                ["index", name, top] => {
                    check_ref_name(name)?;
                    if pointer
                        .indexes
                        .insert((*name).to_owned(), top.parse()?)
                        .is_some()
                    {
                        bail!("index {name}: given twice");
                    }
                }
                ["channel", name, target, rest @ ..] if rest.is_empty() || rest == ["ended"] => {
                    check_ref_name(name)?;
                    let channel = Channel {
                        target: (*target).to_owned(),
                        ended: !rest.is_empty(),
                    };
                    if pointer
                        .channels
                        .insert((*name).to_owned(), channel)
                        .is_some()
                    {
                        bail!("channel {name}: given twice");
                    }
                }
                _ => bail!("{line:?}: not a catalog, index or channel"),
            }
        }
        pointer.check()?;
        if pointer.render() != text {
            bail!("not in its canonical order and form");
        }
        Ok(pointer)
    }

    /// Check that no name is both an index and a channel, and that every
    /// channel reaches an index.
    pub fn check(&self) -> Result<()> {
        if let Some(name) = self
            .channels
            .keys()
            .find(|name| self.indexes.contains_key(*name))
        {
            bail!("{name}: both an index and a channel");
        }
        for name in self.channels.keys() {
            self.index_of(name)?;
        }
        Ok(())
    }

    /// The index `name` is, or that the channel `name` reaches, following
    /// channels of channels.
    pub fn index_of(&self, name: &str) -> Result<(&str, &Identity)> {
        let mut at = name;
        for _ in 0..=self.channels.len() {
            if let Some((index, top)) = self.indexes.get_key_value(at) {
                return Ok((index, top));
            }
            match self.channels.get(at) {
                Some(channel) => at = &channel.target,
                None if at == name => bail!("{name}: no such index or channel"),
                None => bail!("channel {name}: follows {at}, which is no index or channel"),
            }
        }
        bail!("channel {name}: follows a loop of channels")
    }

    /// The pointer as the next publication: one more in sequence, signed
    /// now.
    pub fn next(&self) -> Self {
        Self {
            sequence: self.sequence + 1,
            signed: now(),
            ..self.clone()
        }
    }
}

/// Check that `name` can name an index or channel: 1 to 64 ASCII letters,
/// digits and `-`, `.` and `_`, not starting with a dot.
pub fn check_ref_name(name: &str) -> Result<()> {
    let valid = (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-._".contains(&c))
        && !name.starts_with('.');
    if !valid {
        bail!(
            "invalid index or channel name '{name}': use 1 to 64 ASCII letters, digits, \
             - . and _, not starting with a dot"
        );
    }
    Ok(())
}

fn check_time(time: &str) -> Result<()> {
    let b = time.as_bytes();
    let digits = |range: std::ops::Range<usize>| b[range].iter().all(u8::is_ascii_digit);
    let valid = b.len() == 20
        && digits(0..4)
        && b[4] == b'-'
        && digits(5..7)
        && b[7] == b'-'
        && digits(8..10)
        && b[10] == b'T'
        && digits(11..13)
        && b[13] == b':'
        && digits(14..16)
        && b[16] == b':'
        && digits(17..19)
        && b[19] == b'Z';
    if !valid {
        bail!("{time:?}: not a time as YYYY-MM-DDTHH:MM:SSZ");
    }
    Ok(())
}

/// The time now, as `YYYY-MM-DDTHH:MM:SSZ`.
fn now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    format_time(seconds)
}

/// The UTC time `seconds` after the epoch, as `YYYY-MM-DDTHH:MM:SSZ`.
fn format_time(seconds: u64) -> String {
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX);
    let rest = seconds % 86_400;
    // The proleptic Gregorian date of a day count, in eras of 400 years.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest / 60 % 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_utc_dates() {
        assert_eq!(format_time(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_time(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_time(1_794_571_200), "2026-11-13T12:00:00Z");
    }

    #[test]
    fn a_pointer_reads_back_and_channels_reach_indexes() {
        let mut pointer = Pointer {
            sequence: 7,
            signed: "2026-11-13T12:00:00Z".to_owned(),
            catalog: Some(Identity::of(b"catalog")),
            ..Pointer::default()
        };
        pointer
            .indexes
            .insert(LEADING.to_owned(), Identity::of(b"leading"));
        pointer
            .indexes
            .insert("2026.10.0".to_owned(), Identity::of(b"snapshot"));
        for (name, target, ended) in [
            ("2026.10", "2026.10.0", false),
            ("stable", "2026.10", false),
            ("unstable", LEADING, false),
            ("2026.09", "2026.10.0", true),
        ] {
            pointer.channels.insert(
                name.to_owned(),
                Channel {
                    target: target.to_owned(),
                    ended,
                },
            );
        }
        let text = pointer.render();
        assert_eq!(Pointer::parse(&text).unwrap(), pointer);
        assert_eq!(pointer.index_of("stable").unwrap().0, "2026.10.0");
        assert!(text.contains("channel 2026.09 2026.10.0 ended\n"), "{text}");
        assert_eq!(pointer.next().sequence, 8);

        let mut looped = pointer.clone();
        looped.channels.get_mut("2026.10").unwrap().target = "stable".to_owned();
        assert!(looped.check().is_err());
        for bad in [
            text.replace("sequence 7", "sequence x"),
            text.replace("2026-11-13T12:00:00Z", "yesterday"),
            text.replace("channel stable 2026.10", "channel stable nowhere"),
            format!("{text}index leading {}\n", Identity::of(b"again")),
            text.replace("index 2026.10.0", "index .hidden"),
        ] {
            assert!(Pointer::parse(&bad).is_err(), "{bad}");
        }
    }
}
