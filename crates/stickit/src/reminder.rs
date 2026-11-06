//! Natural-language reminders typed inline as `@` tokens.
//!
//! Examples: `@3pm`, `@15:30`, `@in 20m`, `@in 1h30m`, `@tomorrow 9am`, `@fri 2pm`,
//! `@2026-12-01 10:00`, `@tonight`, `@daily 9am`, `@weekdays 8:30`, `@every 45m`,
//! `@every mon 10am`. A token is consumed (removed from the text and attached to the note)
//! when the line is finished with Enter or the note loses focus.

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, NaiveTime, TimeZone, Weekday};
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repeat {
    Once,
    Daily,
    Weekdays,
    Weekly,
    Every(i64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reminder {
    /// Next time to ring (unix seconds). Differs from `anchor` only while snoozed.
    pub due: i64,
    /// The scheduled occurrence that recurring reminders advance from.
    pub anchor: i64,
    pub repeat: Repeat,
}

impl Repeat {
    pub fn encode(self) -> String {
        match self {
            Repeat::Once => String::new(),
            Repeat::Daily => "daily".into(),
            Repeat::Weekdays => "weekdays".into(),
            Repeat::Weekly => "weekly".into(),
            Repeat::Every(s) => format!("every:{s}"),
        }
    }

    pub fn decode(s: &str) -> Repeat {
        match s {
            "daily" => Repeat::Daily,
            "weekdays" => Repeat::Weekdays,
            "weekly" => Repeat::Weekly,
            _ => s
                .strip_prefix("every:")
                .and_then(|n| n.parse().ok())
                .filter(|&n: &i64| n > 0)
                .map_or(Repeat::Once, Repeat::Every),
        }
    }
}

impl Repeat {
    /// The choices in the reminder menu, in order.
    pub const CHOICES: [(Repeat, &'static str); 4] =
        [(Repeat::Once, "Once"), (Repeat::Daily, "Daily"), (Repeat::Weekdays, "Weekdays"), (Repeat::Weekly, "Weekly")];
}

impl Reminder {
    fn at(due: DateTime<Local>, repeat: Repeat) -> Self {
        Self { due: due.timestamp(), anchor: due.timestamp(), repeat }
    }

    pub fn once(due: i64) -> Self {
        Self { due, anchor: due, repeat: Repeat::Once }
    }

    pub fn repeating(self, repeat: Repeat) -> Self {
        Self { anchor: self.due, repeat, ..self }
    }

    /// Long form for the reminder menu: "Tomorrow, 9 AM", "Fri 7 Nov, 3:30 PM · every Friday".
    pub fn describe(&self, now: DateTime<Local>) -> String {
        let due = local(self.due);
        let today = now.date_naive();
        let day = if due.date_naive() == today {
            "Today".to_string()
        } else if Some(due.date_naive()) == today.succ_opt() {
            "Tomorrow".to_string()
        } else {
            due.format("%a %-d %b").to_string()
        };
        let repeat = match self.repeat {
            Repeat::Once => String::new(),
            Repeat::Daily => " · every day".into(),
            Repeat::Weekdays => " · every weekday".into(),
            Repeat::Weekly => format!(" · every {}", due.format("%A")),
            Repeat::Every(s) => format!(" · every {}", duration_label(s)),
        };
        format!("{day}, {}{repeat}", clock_label(due.time()))
    }

    /// After a ring is dismissed: the following occurrence, or None for one-shot reminders.
    pub fn advance(&self, now: i64) -> Option<Reminder> {
        let mut next = self.anchor;
        loop {
            next = match self.repeat {
                Repeat::Once => return None,
                Repeat::Every(secs) => next + secs,
                Repeat::Daily => shift_days(next, 1),
                Repeat::Weekly => shift_days(next, 7),
                Repeat::Weekdays => {
                    let mut n = shift_days(next, 1);
                    while matches!(local(n).weekday(), Weekday::Sat | Weekday::Sun) {
                        n = shift_days(n, 1);
                    }
                    n
                }
            };
            if next > now {
                return Some(Reminder { due: next, anchor: next, repeat: self.repeat });
            }
        }
    }

    /// Compact label for the title-bar badge, e.g. "Tmrw 9:30 AM", "Daily 9 AM".
    pub fn label(&self, now: DateTime<Local>) -> String {
        use chrono::Timelike;
        let due = local(self.due);
        let time = due.format(if due.minute() == 0 { "%-I %p" } else { "%-I:%M %p" }).to_string();
        let prefix = match self.repeat {
            Repeat::Daily => return format!("Daily {time}"),
            Repeat::Weekdays => return format!("Weekdays {time}"),
            Repeat::Weekly => return format!("{}s {time}", due.format("%a")),
            Repeat::Every(s) => return format!("Every {}", duration_label(s)),
            Repeat::Once => due.date_naive(),
        };
        let today = now.date_naive();
        if prefix == today {
            time
        } else if Some(prefix) == today.succ_opt() {
            format!("Tmrw {time}")
        } else if prefix > today && prefix - today < Duration::days(7) {
            format!("{} {time}", due.format("%a"))
        } else {
            format!("{} {time}", due.format("%b %-d"))
        }
    }
}

pub fn local(ts: i64) -> DateTime<Local> {
    Local.timestamp_opt(ts, 0).single().unwrap_or_else(Local::now)
}

/// Adds calendar days keeping the wall-clock time (DST-safe).
fn shift_days(ts: i64, days: i64) -> i64 {
    let dt = local(ts);
    let date = dt.date_naive() + Duration::days(days);
    resolve(date, dt.time()).map_or(ts + days * 86_400, |d| d.timestamp())
}

fn resolve(date: NaiveDate, time: NaiveTime) -> Option<DateTime<Local>> {
    Local.from_local_datetime(&date.and_time(time)).earliest()
}

/// "9 AM", "3:30 PM".
pub fn clock_label(t: NaiveTime) -> String {
    use chrono::Timelike;
    t.format(if t.minute() == 0 { "%-I %p" } else { "%-I:%M %p" }).to_string()
}

/// A time of day as written in stickit.ini: "9am", "3:30pm".
pub fn clock_setting(t: NaiveTime) -> String {
    use chrono::Timelike;
    t.format(if t.minute() == 0 { "%-I%P" } else { "%-I:%M%P" }).to_string()
}

/// "3pm", "15:30", "noon" → a time of day (the whole text must be a time).
pub fn clock(text: &str) -> Option<NaiveTime> {
    let words: Vec<String> = text.split_whitespace().map(|w| w.to_ascii_lowercase()).collect();
    let w: Vec<&str> = words.iter().map(String::as_str).collect();
    time_of_day(&w).filter(|(_, n)| *n == w.len()).map(|(t, _)| t)
}

/// "20m", "1h30m", "2 hours" → seconds (the whole text must be a duration).
pub fn span(text: &str) -> Option<i64> {
    let words: Vec<String> = text.split_whitespace().map(|w| w.to_ascii_lowercase()).collect();
    let w: Vec<&str> = words.iter().map(String::as_str).collect();
    duration(&w).filter(|(_, n)| *n == w.len()).map(|(s, _)| s)
}

/// A starting time for the reminder menu's fine-tune row: an hour from now, rounded up to a
/// multiple of `step` minutes.
pub fn suggested_time(now: DateTime<Local>, step: u32) -> NaiveTime {
    use chrono::Timelike;
    let step = step.clamp(1, 60);
    let later = now.time().overflowing_add_signed(Duration::minutes(60)).0;
    let minutes = later.hour() * 60 + later.minute();
    let rounded = minutes.div_ceil(step) * step % (24 * 60);
    NaiveTime::from_hms_opt(rounded / 60, rounded % 60, 0).unwrap_or(later)
}

/// `time` moved by `minutes` (wrapping around midnight).
pub fn nudge(time: NaiveTime, minutes: i64) -> NaiveTime {
    time.overflowing_add_signed(Duration::minutes(minutes)).0
}

/// `offset` days from today at `time`, if that is still ahead.
pub fn at_day(now: DateTime<Local>, offset: i64, time: NaiveTime) -> Option<i64> {
    let dt = resolve(now.date_naive() + Duration::days(offset), time)?;
    (dt > now).then(|| dt.timestamp())
}

/// Whole days from today to `ts`.
pub fn day_offset(ts: i64, now: DateTime<Local>) -> i64 {
    (local(ts).date_naive() - now.date_naive()).num_days()
}

pub fn duration_label(secs: i64) -> String {
    match secs {
        s if s % 86_400 == 0 => format!("{}d", s / 86_400),
        s if s % 3600 == 0 => format!("{}h", s / 3600),
        s if s >= 3600 => format!("{}h{}m", s / 3600, s % 3600 / 60),
        s => format!("{}m", s / 60),
    }
}

/// A recognised token: its byte range in the text (including `@`) and the reminder.
#[derive(Debug)]
pub struct Token {
    pub range: Range<usize>,
    pub reminder: Reminder,
}

/// Finds every reminder token in `text`, skipping fenced code blocks.
pub fn find_tokens(text: &str, now: DateTime<Local>) -> Vec<Token> {
    let mut out = Vec::new();
    let mut in_fence = false;
    let mut start = 0;
    for line in text.split('\n') {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        } else if !in_fence {
            let b = line.as_bytes();
            for (i, _) in line.match_indices('@') {
                let in_code = line[..i].matches('`').count() % 2 == 1;
                if in_code || (i > 0 && !b[i - 1].is_ascii_whitespace()) {
                    continue;
                }
                if let Some((reminder, len)) = parse(&line[i + 1..], now) {
                    out.push(Token { range: start + i..start + i + 1 + len, reminder });
                }
            }
        }
        start += line.len() + 1;
    }
    out
}

/// Parses the text following `@`. Returns the reminder and how many bytes were consumed.
pub fn parse(spec: &str, now: DateTime<Local>) -> Option<(Reminder, usize)> {
    let words: Vec<(usize, String)> = spec
        .split_whitespace()
        .take(4)
        .map(|w| (w.as_ptr() as usize - spec.as_ptr() as usize, w.to_ascii_lowercase()))
        .collect();
    if words.is_empty() || words[0].0 != 0 {
        return None;
    }
    let w: Vec<&str> = words.iter().map(|(_, s)| s.as_str()).collect();
    let (reminder, used) = parse_words(&w, now)?;
    let (off, word) = &words[used - 1];
    Some((reminder, off + word.len()))
}

fn parse_words(w: &[&str], now: DateTime<Local>) -> Option<(Reminder, usize)> {
    let today = now.date_naive();
    let nine = NaiveTime::from_hms_opt(9, 0, 0)?;

    // Relative: "in 20m", "in 1h30m", "in 2 hours".
    if w[0] == "in" {
        let (secs, n) = duration(&w[1..])?;
        return Some((Reminder::at(now + Duration::seconds(secs), Repeat::Once), 1 + n));
    }

    // Recurring.
    let recurring = match w[0] {
        "daily" => Some((Repeat::Daily, 1)),
        "weekdays" => Some((Repeat::Weekdays, 1)),
        "every" => match w.get(1).copied() {
            Some("day") => Some((Repeat::Daily, 2)),
            Some("weekday") => Some((Repeat::Weekdays, 2)),
            Some(d) if weekday(d).is_some() => Some((Repeat::Weekly, 1)),
            Some(_) => {
                let (secs, n) = duration(&w[1..])?;
                let r = Reminder::at(now + Duration::seconds(secs), Repeat::Every(secs));
                return Some((r, 1 + n));
            }
            None => None,
        },
        _ => None,
    };
    if let Some((repeat, used)) = recurring {
        let rest = &w[used..];
        let (day, day_used) = match rest.first().and_then(|d| weekday(d)) {
            Some(wd) => (Some(wd), 1),
            None => (None, 0),
        };
        let (time, time_used) = time_of_day(&rest[day_used..]).unwrap_or((nine, 0));
        let mut date = today;
        loop {
            let ok = match (repeat, day) {
                (_, Some(wd)) => date.weekday() == wd,
                (Repeat::Weekdays, _) => !matches!(date.weekday(), Weekday::Sat | Weekday::Sun),
                _ => true,
            };
            if ok {
                if let Some(dt) = resolve(date, time).filter(|dt| *dt > now) {
                    return Some((Reminder::at(dt, repeat), used + day_used + time_used));
                }
            }
            date = date.succ_opt()?;
        }
    }

    // One-shot with an optional day part.
    let (date, used) = match w[0] {
        "today" => (Some(today), 1),
        "tonight" => {
            let t = time_of_day(&w[1..]).map_or(NaiveTime::from_hms_opt(20, 0, 0)?, |(t, _)| t);
            let used = 1 + time_of_day(&w[1..]).map_or(0, |(_, n)| n);
            let dt = resolve(today, t)?;
            return (dt > now).then(|| (Reminder::at(dt, Repeat::Once), used));
        }
        "tomorrow" | "tmr" | "tmrw" => (today.succ_opt(), 1),
        d => match weekday(d) {
            Some(wd) => {
                let ahead = (7 + wd.num_days_from_monday() as i64 - today.weekday().num_days_from_monday() as i64) % 7;
                (Some(today + Duration::days(if ahead == 0 { 7 } else { ahead })), 1)
            }
            None => match NaiveDate::parse_from_str(d, "%Y-%m-%d") {
                Ok(date) => (Some(date), 1),
                Err(_) => (None, 0),
            },
        },
    };

    let rest = &w[used..];
    match (date, time_of_day(rest)) {
        (Some(date), Some((time, n))) => {
            let dt = resolve(date, time)?;
            (dt > now).then(|| (Reminder::at(dt, Repeat::Once), used + n))
        }
        (Some(date), None) => {
            let dt = resolve(date, nine)?;
            (dt > now).then(|| (Reminder::at(dt, Repeat::Once), used))
        }
        (None, _) => {
            // Bare time: today, or tomorrow if it has already passed.
            let (time, n) = time_of_day(w)?;
            let mut dt = resolve(today, time)?;
            if dt <= now {
                dt = resolve(today.succ_opt()?, time)?;
            }
            Some((Reminder::at(dt, Repeat::Once), n))
        }
    }
}

fn weekday(w: &str) -> Option<Weekday> {
    Some(match w {
        "mon" | "monday" => Weekday::Mon,
        "tue" | "tues" | "tuesday" => Weekday::Tue,
        "wed" | "wednesday" => Weekday::Wed,
        "thu" | "thur" | "thurs" | "thursday" => Weekday::Thu,
        "fri" | "friday" => Weekday::Fri,
        "sat" | "saturday" => Weekday::Sat,
        "sun" | "sunday" => Weekday::Sun,
        _ => return None,
    })
}

/// "3pm", "3:30pm", "15:30", "3 pm", "at 3pm", "noon", "midnight".
fn time_of_day(w: &[&str]) -> Option<(NaiveTime, usize)> {
    let (w, skip) = match w.first() {
        Some(&"at") => (&w[1..], 1),
        _ => (w, 0),
    };
    let first = *w.first()?;
    match first {
        "noon" => return Some((NaiveTime::from_hms_opt(12, 0, 0)?, skip + 1)),
        "midnight" => return Some((NaiveTime::from_hms_opt(0, 0, 0)?, skip + 1)),
        _ => {}
    }
    let (body, suffix, used) = if let Some(b) = first.strip_suffix("am") {
        (b, Some(false), 1)
    } else if let Some(b) = first.strip_suffix("pm") {
        (b, Some(true), 1)
    } else {
        match w.get(1).copied() {
            Some("am") => (first, Some(false), 2),
            Some("pm") => (first, Some(true), 2),
            _ => (first, None, 1),
        }
    };
    let (h, m) = match body.split_once(':') {
        Some((h, m)) if m.len() == 2 => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        Some(_) => return None,
        // A bare number is only a time with am/pm ("@3" is too ambiguous).
        None if suffix.is_some() => (body.parse::<u32>().ok()?, 0),
        None => return None,
    };
    let h = match suffix {
        Some(pm) if (1..=12).contains(&h) => h % 12 + if pm { 12 } else { 0 },
        Some(_) => return None,
        None => h,
    };
    Some((NaiveTime::from_hms_opt(h, m, 0)?, skip + used))
}

/// "20m", "1h30m", "2h", "1d", "90s", or two words like "20 min", "2 hours".
fn duration(w: &[&str]) -> Option<(i64, usize)> {
    let first = *w.first()?;
    if let Ok(n) = first.parse::<i64>() {
        let unit = unit_secs(w.get(1)?)?;
        return Some((n * unit, 2)).filter(|(s, _)| *s > 0);
    }
    let mut total = 0;
    let mut num = String::new();
    let mut chars = first.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            let mut unit = c.to_string();
            while chars.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
                unit.push(chars.next()?);
            }
            total += num.parse::<i64>().ok()? * unit_secs(&unit)?;
            num.clear();
        }
    }
    (num.is_empty() && total > 0).then_some((total, 1))
}

fn unit_secs(u: &str) -> Option<i64> {
    Some(match u {
        "s" | "sec" | "secs" | "second" | "seconds" => 1,
        "m" | "min" | "mins" | "minute" | "minutes" => 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3600,
        "d" | "day" | "days" => 86_400,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Local> {
        // Wednesday 2026-11-04 10:00 local.
        Local.with_ymd_and_hms(2026, 11, 4, 10, 0, 0).unwrap()
    }

    #[test]
    fn menu_helpers() {
        assert_eq!(clock("3:30pm"), NaiveTime::from_hms_opt(15, 30, 0));
        assert_eq!(clock("3pm tomorrow"), None);
        assert_eq!(span("1h30m"), Some(5400));
        assert_eq!(clock_setting(NaiveTime::from_hms_opt(21, 0, 0).unwrap()), "9pm");
        // 9 AM today has passed at 10:00; tomorrow's hasn't.
        let nine = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        assert_eq!(at_day(now(), 0, nine), None);
        let t = at_day(now(), 1, nine).unwrap();
        assert_eq!(day_offset(t, now()), 1);
        let r = Reminder::once(t).repeating(Repeat::Weekly);
        assert_eq!(r.describe(now()), "Tomorrow, 9 AM · every Thursday");
    }

    fn due(spec: &str) -> (DateTime<Local>, Repeat, usize) {
        let (r, len) = parse(spec, now()).unwrap_or_else(|| panic!("no parse: {spec}"));
        (local(r.due), r.repeat, len)
    }

    fn hm(d: DateTime<Local>) -> (u32, u32, u32) {
        (d.day(), d.hour(), d.minute())
    }

    use chrono::Timelike;

    #[test]
    fn times() {
        assert_eq!(hm(due("3pm").0), (4, 15, 0));
        assert_eq!(hm(due("3:30 pm and more").0), (4, 15, 30));
        assert_eq!(due("3:30 pm and more").2, "3:30 pm".len());
        assert_eq!(hm(due("9am").0), (5, 9, 0)); // already passed → tomorrow
        assert_eq!(hm(due("18:45").0), (4, 18, 45));
        assert!(parse("3", now()).is_none());
        assert!(parse("john", now()).is_none());
    }

    #[test]
    fn relative() {
        assert_eq!(hm(due("in 20m").0), (4, 10, 20));
        assert_eq!(hm(due("in 1h30m").0), (4, 11, 30));
        assert_eq!(hm(due("in 2 hours").0), (4, 12, 0));
        assert_eq!(due("in 2 hours call").2, "in 2 hours".len());
    }

    #[test]
    fn days() {
        assert_eq!(hm(due("tomorrow").0), (5, 9, 0));
        assert_eq!(hm(due("tomorrow at 2pm").0), (5, 14, 0));
        assert_eq!(hm(due("fri 4pm").0), (6, 16, 0));
        assert_eq!(hm(due("wed").0), (11, 9, 0)); // same weekday → next week
        assert_eq!(hm(due("tonight").0), (4, 20, 0));
        assert_eq!(hm(due("2026-12-01 10:15").0), (1, 10, 15));
    }

    #[test]
    fn fine_tune_times() {
        let t = |h, m| NaiveTime::from_hms_opt(h, m, 0).unwrap();
        assert_eq!(nudge(t(9, 0), -5), t(8, 55));
        assert_eq!(nudge(t(23, 30), 60), t(0, 30));
        // An hour from 10:00, on the step.
        assert_eq!(suggested_time(now(), 5), t(11, 0));
        assert_eq!(suggested_time(now() + Duration::minutes(7), 5), t(11, 10));
        assert_eq!(suggested_time(now() + Duration::minutes(7), 15), t(11, 15));
        assert_eq!(suggested_time(now() + Duration::minutes(7), 1), t(11, 7));
    }

    #[test]
    fn recurring() {
        let (d, r, _) = due("daily 9am");
        assert_eq!((hm(d), r), ((5, 9, 0), Repeat::Daily));
        let (d, r, _) = due("weekdays 8:30am");
        assert_eq!((hm(d), r), ((5, 8, 30), Repeat::Weekdays));
        let (d, r, _) = due("every mon 10am");
        assert_eq!((hm(d), r), ((9, 10, 0), Repeat::Weekly));
        let (d, r, _) = due("every 45m");
        assert_eq!((hm(d), r), ((4, 10, 45), Repeat::Every(2700)));
    }

    #[test]
    fn advance_skips_weekend() {
        // Friday 2026-11-06 8:30 → Monday 9th.
        let fri = Local.with_ymd_and_hms(2026, 11, 6, 8, 30, 0).unwrap().timestamp();
        let r = Reminder { due: fri, anchor: fri, repeat: Repeat::Weekdays };
        let next = r.advance(fri).unwrap();
        assert_eq!(hm(local(next.due)), (9, 8, 30));
        let once = Reminder { repeat: Repeat::Once, ..r };
        assert!(once.advance(fri).is_none());
    }

    #[test]
    fn tokens_need_leading_space() {
        let t = "mail a@b.com\ncall mom @3pm ok `@5pm`\n```\n@4pm\n```";
        let toks = find_tokens(t, now());
        assert_eq!(toks.len(), 1);
        assert_eq!(&t[toks[0].range.clone()], "@3pm");
    }

    #[test]
    fn labels_are_compact() {
        let label = |spec: &str| parse(spec, now()).unwrap().0.label(now());
        assert_eq!(label("3pm"), "3 PM");
        assert_eq!(label("tomorrow 9:30am"), "Tmrw 9:30 AM");
        assert_eq!(label("fri 4pm"), "Fri 4 PM");
        assert_eq!(label("daily 9am"), "Daily 9 AM");
        assert_eq!(label("every 45m"), "Every 45m");
    }

    #[test]
    fn repeat_roundtrip() {
        for r in [Repeat::Once, Repeat::Daily, Repeat::Weekdays, Repeat::Weekly, Repeat::Every(60)] {
            assert_eq!(Repeat::decode(&r.encode()), r);
        }
    }
}
