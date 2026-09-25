//! Daily notes (SPEC §9): where a day's note lives, per vault.
//!
//! The settings live in the vault doc's `meta` map ([`crate::vault_doc::VaultDoc::daily`]), so
//! every replica — server, relay, browser — files a day under the same path. The date format is
//! Obsidian's, which is Moment.js's, because that is what an imported `daily-notes.json` holds.
//! `ui/src/lib/daily.ts` is the TypeScript twin; `corpus/daily-formats.json` keeps them agreeing.

use serde::{Deserialize, Serialize};

pub const DEFAULT_FOLDER: &str = "Daily";
pub const DEFAULT_FORMAT: &str = "YYYY-MM-DD";
pub const DEFAULT_TEMPLATE: &str = "Templates/Daily.md";

/// Where daily notes go and what they are called. An empty field means the default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DailySettings {
    /// Folder new daily notes go into ("" = the default, `Daily`; "/" = the vault root).
    #[serde(default)]
    pub folder: String,
    /// Moment.js-style date format for the file name, which may itself contain `/`.
    #[serde(default)]
    pub format: String,
    /// Template applied when the note is created, vault-relative.
    #[serde(default)]
    pub template: String,
}

impl DailySettings {
    pub fn folder(&self) -> &str {
        match self.folder.trim().trim_matches('/') {
            "" if self.folder.trim() == "/" => "",
            "" => DEFAULT_FOLDER,
            f => f,
        }
    }

    pub fn format(&self) -> &str {
        match self.format.trim() {
            "" => DEFAULT_FORMAT,
            f => f,
        }
    }

    pub fn template(&self) -> &str {
        match self.template.trim() {
            "" => DEFAULT_TEMPLATE,
            t => t,
        }
    }

    /// Settings from an Obsidian `daily-notes.json`: `folder`, `format`, `template` (the last
    /// without its `.md`). Missing keys stay empty, i.e. default. An Obsidian vault with no folder
    /// set keeps its daily notes at the root, so an empty `folder` there means the root.
    pub fn from_obsidian(raw: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(raw).ok()?;
        let s = |k: &str| value.get(k).and_then(|v| v.as_str()).unwrap_or("").trim().to_owned();
        let folder = s("folder");
        let template = s("template");
        Some(Self {
            folder: if folder.is_empty() { "/".into() } else { folder },
            format: s("format"),
            template: if template.is_empty() || template.ends_with(".md") || template.ends_with(".qmd") {
                template
            } else {
                format!("{template}.md")
            },
        })
    }

    /// The vault-relative path of the note for `date`.
    pub fn path_for(&self, date: Date) -> String {
        let name = format_date(self.format(), date);
        match self.folder() {
            "" => format!("{name}.md"),
            f => format!("{f}/{name}.md"),
        }
    }
}

/// A calendar date (proleptic Gregorian).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

impl Date {
    /// `YYYY-MM-DD`, validated.
    pub fn parse(s: &str) -> Option<Self> {
        let mut it = s.split('-');
        let (y, m, d) = (it.next()?, it.next()?, it.next()?);
        if it.next().is_some() || y.len() != 4 || m.len() != 2 || d.len() != 2 {
            return None;
        }
        let date = Self { year: y.parse().ok()?, month: m.parse().ok()?, day: d.parse().ok()? };
        (date.month >= 1
            && date.month <= 12
            && date.day >= 1
            && date.day <= days_in_month(date.year, date.month))
        .then_some(date)
    }

    pub fn iso(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// Days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
    fn days(self) -> i64 {
        let y = i64::from(self.year) - i64::from(self.month <= 2);
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let m = i64::from(self.month);
        let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(self.day) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    fn from_days(z: i64) -> Self {
        let z = z + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
        Self { year, month, day }
    }

    fn add_days(self, n: i64) -> Self {
        Self::from_days(self.days() + n)
    }

    /// 0 = Sunday … 6 = Saturday.
    fn weekday(self) -> u32 {
        (self.days() + 4).rem_euclid(7) as u32
    }

    /// 1-based day of the year.
    fn ordinal(self) -> u32 {
        (self.days() - Self { year: self.year, month: 1, day: 1 }.days()) as u32 + 1
    }

    /// ISO 8601 week-numbering year and week.
    fn iso_week(self) -> (i32, u32) {
        // The Thursday of this date's Monday-started week decides both.
        let iso_wd = (self.weekday() + 6) % 7; // Monday = 0
        let thursday = self.add_days(3 - i64::from(iso_wd));
        (thursday.year, (thursday.ordinal() - 1) / 7 + 1)
    }

    /// Moment's `en` locale week: weeks start on Sunday and week 1 holds 1 January.
    fn locale_week(self) -> (i32, u32) {
        let saturday = self.add_days(6 - i64::from(self.weekday()));
        (saturday.year, (saturday.ordinal() - 1) / 7 + 1)
    }
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => 31,
    }
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const WEEKDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

/// Moment's tokens, longest first so that `MMMM` wins over `MM`. Anything else is copied
/// through, and `[…]` escapes literal text.
const TOKENS: [&str; 26] = [
    "YYYY", "GGGG", "gggg", "MMMM", "DDDD", "dddd", "MMM", "DDD", "ddd", "YY", "GG", "gg", "MM", "DD", "Do",
    "dd", "WW", "ww", "Q", "M", "D", "d", "E", "e", "W", "w",
];

/// Format a date with a Moment.js format string (the subset that makes sense for a date).
pub fn format_date(format: &str, date: Date) -> String {
    let mut out = String::new();
    let mut rest = format;
    while let Some(c) = rest.chars().next() {
        // An unterminated `[` is an ordinary character, as it is to Moment.
        if c == '['
            && let Some(end) = rest[1..].find(']')
        {
            out.push_str(&rest[1..1 + end]);
            rest = &rest[end + 2..];
            continue;
        }
        match TOKENS.iter().find(|t| rest.starts_with(**t)) {
            Some(t) => {
                out.push_str(&token(t, date));
                rest = &rest[t.len()..];
            }
            None => {
                out.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    out
}

fn token(t: &str, d: Date) -> String {
    let month = MONTHS[d.month as usize - 1];
    let weekday = WEEKDAYS[d.weekday() as usize];
    match t {
        "YYYY" => format!("{:04}", d.year),
        "YY" => format!("{:02}", d.year.rem_euclid(100)),
        "Q" => ((d.month - 1) / 3 + 1).to_string(),
        "MMMM" => month.to_owned(),
        "MMM" => month[..3].to_owned(),
        "MM" => format!("{:02}", d.month),
        "M" => d.month.to_string(),
        "DDDD" => format!("{:03}", d.ordinal()),
        "DDD" => d.ordinal().to_string(),
        "DD" => format!("{:02}", d.day),
        "D" => d.day.to_string(),
        "Do" => {
            let suffix = match (d.day % 10, d.day % 100) {
                (_, 11..=13) => "th",
                (1, _) => "st",
                (2, _) => "nd",
                (3, _) => "rd",
                _ => "th",
            };
            format!("{}{suffix}", d.day)
        }
        "dddd" => weekday.to_owned(),
        "ddd" => weekday[..3].to_owned(),
        "dd" => weekday[..2].to_owned(),
        "d" | "e" => d.weekday().to_string(),
        "E" => ((d.weekday() + 6) % 7 + 1).to_string(),
        "GGGG" => format!("{:04}", d.iso_week().0),
        "GG" => format!("{:02}", d.iso_week().0.rem_euclid(100)),
        "WW" => format!("{:02}", d.iso_week().1),
        "W" => d.iso_week().1.to_string(),
        "gggg" => format!("{:04}", d.locale_week().0),
        "gg" => format!("{:02}", d.locale_week().0.rem_euclid(100)),
        "ww" => format!("{:02}", d.locale_week().1),
        "w" => d.locale_week().1.to_string(),
        _ => t.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(s: &str) -> Date {
        Date::parse(s).unwrap()
    }

    #[test]
    fn dates_parse_strictly() {
        assert_eq!(date("2026-09-25"), Date { year: 2026, month: 9, day: 25 });
        assert_eq!(Date::parse("2026-02-29"), None);
        assert!(Date::parse("2024-02-29").is_some());
        assert_eq!(Date::parse("2026-9-25"), None);
        assert_eq!(Date::parse("../etc/pw"), None);
        assert_eq!(date("2026-09-25").iso(), "2026-09-25");
    }

    #[test]
    fn day_arithmetic_round_trips() {
        for s in ["1970-01-01", "2000-02-29", "2026-12-31", "1899-03-01"] {
            let d = date(s);
            assert_eq!(Date::from_days(d.days()), d);
        }
        assert_eq!(date("1970-01-01").weekday(), 4); // Thursday
        assert_eq!(date("2026-09-25").weekday(), 5); // Friday
    }

    #[test]
    fn defaults_and_the_root_folder() {
        let s = DailySettings::default();
        assert_eq!(s.path_for(date("2026-09-25")), "Daily/2026-09-25.md");
        let root = DailySettings { folder: "/".into(), ..Default::default() };
        assert_eq!(root.path_for(date("2026-09-25")), "2026-09-25.md");
        let nested = DailySettings {
            folder: "/Journal/".into(),
            format: "YYYY/MM/DD dddd".into(),
            ..Default::default()
        };
        assert_eq!(nested.path_for(date("2026-09-25")), "Journal/2026/09/25 Friday.md");
    }

    #[test]
    fn obsidian_settings_translate() {
        let s =
            DailySettings::from_obsidian(r#"{"folder":"Journal","format":"DD.MM.YYYY","template":"T/Day"}"#)
                .unwrap();
        assert_eq!(s.path_for(date("2026-01-05")), "Journal/05.01.2026.md");
        assert_eq!(s.template(), "T/Day.md");
        // Obsidian's default is the vault root, not our `Daily/`.
        let bare = DailySettings::from_obsidian("{}").unwrap();
        assert_eq!(bare.path_for(date("2026-01-05")), "2026-01-05.md");
        assert_eq!(bare.template(), DEFAULT_TEMPLATE);
        assert_eq!(DailySettings::from_obsidian("not json"), None);
    }

    /// The fixtures the TypeScript formatter is held to as well.
    #[test]
    fn shared_format_fixtures() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/daily-formats.json");
        let cases: Vec<(String, String, String)> =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert!(!cases.is_empty());
        for (format, day, expected) in cases {
            assert_eq!(format_date(&format, date(&day)), expected, "{format} on {day}");
        }
    }
}
