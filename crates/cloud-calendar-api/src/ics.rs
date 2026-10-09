//! Just enough iCalendar (RFC 5545) for CalDAV: reading VEVENTs, writing a new one, and changing
//! one in place so everything cloud-calendar doesn't touch (alarms, attendees, repeat rules, Apple's
//! own properties) is sent back as it came.

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};

use crate::error::{Error, Result};
use crate::types::{EventChange, NewEvent, Time};

#[derive(Debug, Clone, PartialEq)]
pub struct Property {
    pub name: String,
    pub params: Vec<(String, String)>,
    pub value: String,
}

impl Property {
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// One VEVENT as read.
#[derive(Debug, Clone, PartialEq)]
pub struct VEvent {
    pub uid: String,
    pub summary: String,
    pub location: Option<String>,
    pub description: Option<String>,
    pub start: Time,
    pub end: Time,
    /// Carries a repeat rule (the server didn't expand it).
    pub rrule: bool,
    /// One occurrence of a series (an expanded or changed one).
    pub recurrence_id: Option<String>,
    pub cancelled: bool,
}

/// Lines with folding undone (a line starting with a space or tab continues the one before).
pub fn unfold(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(rest) = raw.strip_prefix([' ', '\t'])
            && let Some(last) = lines.last_mut()
        {
            last.push_str(rest);
        } else if !raw.is_empty() {
            lines.push(raw.to_string());
        }
    }
    lines
}

/// `NAME;P=v;Q="v":value`, quotes in parameters respected.
pub fn parse_line(line: &str) -> Option<Property> {
    let mut in_quote = false;
    let mut colon = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quote = !in_quote,
            ':' if !in_quote => {
                colon = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon = colon?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut parts = Vec::new();
    let (mut cur, mut q) = (String::new(), false);
    for c in head.chars() {
        match c {
            '"' => q = !q,
            ';' if !q => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    let name = parts.first()?.trim().to_ascii_uppercase();
    if name.is_empty() {
        return None;
    }
    let params = parts[1..].iter().filter_map(|p| p.split_once('=')).map(|(k, v)| (k.trim().to_ascii_uppercase(), v.to_string())).collect();
    Some(Property { name, params, value: value.to_string() })
}

pub fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn escape(v: &str) -> String {
    v.replace('\\', "\\\\").replace(';', "\\;").replace(',', "\\,").replace("\r\n", "\n").replace('\n', "\\n")
}

/// A content line folded at 75 octets, CRLF-terminated.
pub fn fold(line: &str) -> String {
    let mut out = String::new();
    let mut width = 0;
    for c in line.chars() {
        let len = c.len_utf8();
        if width + len > 75 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(c);
        width += len;
    }
    out.push_str("\r\n");
    out
}

/// A DTSTART/DTEND/RECURRENCE-ID value: a date, a UTC time, a time in a TZID zone, or a floating
/// (local) time.
pub fn parse_time(p: &Property) -> Option<Time> {
    let v = p.value.trim();
    if p.param("VALUE").is_some_and(|x| x.eq_ignore_ascii_case("DATE")) || (v.len() == 8 && v.chars().all(|c| c.is_ascii_digit())) {
        return NaiveDate::parse_from_str(v, "%Y%m%d").ok().map(Time::Date);
    }
    if let Some(utc) = v.strip_suffix('Z') {
        return NaiveDateTime::parse_from_str(utc, "%Y%m%dT%H%M%S").ok().map(|n| Time::At(n.and_utc()));
    }
    let naive = NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S").ok()?;
    let zoned = p.param("TZID").map(|z| z.trim_matches('"').trim_start_matches('/')).and_then(|z| z.parse::<chrono_tz::Tz>().ok());
    let instant = match zoned {
        Some(tz) => tz.from_local_datetime(&naive).earliest().map(|t| t.with_timezone(&Utc)),
        None => Local.from_local_datetime(&naive).earliest().map(|t| t.with_timezone(&Utc)),
    };
    instant.map(Time::At)
}

/// An RFC 5545 duration (`P1D`, `PT1H30M`, `-P1W`) in seconds.
pub fn parse_duration(v: &str) -> Option<i64> {
    let v = v.trim();
    let (sign, v) = match v.strip_prefix('-') {
        Some(r) => (-1, r),
        None => (1, v.strip_prefix('+').unwrap_or(v)),
    };
    let v = v.strip_prefix('P')?;
    let (mut total, mut num, mut time) = (0i64, String::new(), false);
    for c in v.chars() {
        match c {
            '0'..='9' => num.push(c),
            'T' => time = true,
            'W' | 'D' | 'H' | 'M' | 'S' => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                total += n * match (c, time) {
                    ('W', _) => 604_800,
                    ('D', _) => 86_400,
                    ('H', true) => 3_600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => return None,
                };
            }
            _ => return None,
        }
    }
    num.is_empty().then_some(sign * total)
}

/// Every VEVENT in a calendar object. Events without a start are skipped.
pub fn events(text: &str) -> Vec<VEvent> {
    let mut out = Vec::new();
    let mut depth_in_event: Option<usize> = None;
    let mut nested = 0usize;
    let mut props: Vec<Property> = Vec::new();
    for line in unfold(text) {
        let Some(p) = parse_line(&line) else { continue };
        match (p.name.as_str(), depth_in_event) {
            ("BEGIN", None) if p.value.eq_ignore_ascii_case("VEVENT") => {
                depth_in_event = Some(0);
                props.clear();
            }
            ("BEGIN", Some(_)) => nested += 1,
            ("END", Some(_)) if nested > 0 => nested -= 1,
            ("END", Some(_)) if p.value.eq_ignore_ascii_case("VEVENT") => {
                depth_in_event = None;
                if let Some(e) = vevent(&props) {
                    out.push(e);
                }
            }
            (_, Some(_)) if nested == 0 => props.push(p),
            _ => {}
        }
    }
    out
}

fn vevent(props: &[Property]) -> Option<VEvent> {
    let get = |n: &str| props.iter().find(|p| p.name == n);
    let text = |n: &str| get(n).map(|p| unescape(&p.value)).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let start = parse_time(get("DTSTART")?)?;
    let end = get("DTEND")
        .and_then(parse_time)
        .or_else(|| {
            let secs = parse_duration(&get("DURATION")?.value)?;
            Some(match start {
                Time::At(t) => Time::At(t + chrono::Duration::seconds(secs)),
                Time::Date(d) => Time::Date(d + chrono::Duration::days((secs / 86_400).max(1))),
            })
        })
        .unwrap_or(match start {
            Time::At(t) => Time::At(t),
            Time::Date(d) => Time::Date(d + chrono::Days::new(1)),
        });
    Some(VEvent {
        uid: text("UID").unwrap_or_default(),
        summary: text("SUMMARY").unwrap_or_default(),
        location: text("LOCATION"),
        description: text("DESCRIPTION"),
        start,
        end,
        rrule: get("RRULE").is_some(),
        recurrence_id: get("RECURRENCE-ID").map(|p| p.value.trim().to_string()),
        cancelled: text("STATUS").is_some_and(|s| s.eq_ignore_ascii_case("CANCELLED")),
    })
}

fn stamp(now: DateTime<Utc>) -> String {
    now.format("%Y%m%dT%H%M%SZ").to_string()
}

fn time_line(name: &str, t: &Time) -> String {
    match t {
        Time::At(t) => format!("{name}:{}", t.format("%Y%m%dT%H%M%SZ")),
        Time::Date(d) => format!("{name};VALUE=DATE:{}", d.format("%Y%m%d")),
    }
}

/// A calendar object holding one new event.
pub fn new_event(uid: &str, e: &NewEvent, now: DateTime<Utc>) -> String {
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".into(),
        "PRODID:-//cloud-calendar//EN".into(),
        "CALSCALE:GREGORIAN".into(),
        "BEGIN:VEVENT".into(),
        format!("UID:{uid}"),
        format!("DTSTAMP:{}", stamp(now)),
        format!("CREATED:{}", stamp(now)),
        format!("LAST-MODIFIED:{}", stamp(now)),
        time_line("DTSTART", &e.start),
        time_line("DTEND", &e.end),
        format!("SUMMARY:{}", escape(&e.title)),
    ];
    if let Some(l) = e.location.as_deref().filter(|l| !l.trim().is_empty()) {
        lines.push(format!("LOCATION:{}", escape(l)));
    }
    if let Some(n) = e.notes.as_deref().filter(|n| !n.trim().is_empty()) {
        lines.push(format!("DESCRIPTION:{}", escape(n)));
    }
    lines.extend(["END:VEVENT".to_string(), "END:VCALENDAR".into()]);
    lines.iter().map(|l| fold(l)).collect()
}

/// The calendar object with `change` applied to its main VEVENT (the one without a
/// RECURRENCE-ID, i.e. a whole series): every other line is kept as it was.
pub fn change_event(text: &str, change: &EventChange, now: DateTime<Utc>) -> Result<String> {
    let lines = unfold(text);
    // Find the main VEVENT's line range.
    let (mut range, mut begin, mut nested, mut has_rid) = (None, None, 0usize, false);
    for (i, line) in lines.iter().enumerate() {
        let Some(p) = parse_line(line) else { continue };
        match (p.name.as_str(), begin) {
            ("BEGIN", None) if p.value.eq_ignore_ascii_case("VEVENT") => {
                begin = Some(i);
                has_rid = false;
            }
            ("BEGIN", Some(_)) => nested += 1,
            ("END", Some(_)) if nested > 0 => nested -= 1,
            ("END", Some(b)) if p.value.eq_ignore_ascii_case("VEVENT") => {
                if !has_rid && range.is_none() {
                    range = Some((b, i));
                }
                begin = None;
            }
            ("RECURRENCE-ID", Some(_)) if nested == 0 => has_rid = true,
            _ => {}
        }
    }
    let (b, e) = range.ok_or_else(|| Error::bad_request("the event's calendar data has no event to change"))?;
    let current = events(&lines[b..=e].join("\r\n")).into_iter().next().ok_or_else(|| Error::bad_request("the event has no start"))?;
    let (start, end) = if change.moves() { crate::provider::changed_span(change, current.start, current.end)? } else { (current.start, current.end) };

    let replaced: &[&str] = &["SUMMARY", "LOCATION", "DESCRIPTION", "DTSTAMP", "LAST-MODIFIED", "SEQUENCE"];
    let moved: &[&str] = &["DTSTART", "DTEND", "DURATION"];
    let mut body: Vec<String> = Vec::new();
    let mut sequence = 0i64;
    let mut depth = 0usize;
    for line in &lines[b + 1..e] {
        let p = parse_line(line);
        let name = p.as_ref().map(|p| p.name.as_str()).unwrap_or("");
        if name == "BEGIN" {
            depth += 1;
        }
        if name == "END" {
            depth = depth.saturating_sub(1);
        }
        let top = depth == 0 && name != "END";
        if top && name == "SEQUENCE" {
            sequence = p.as_ref().and_then(|p| p.value.trim().parse().ok()).unwrap_or(0);
        }
        let drop = top
            && ((name == "SUMMARY" && change.title.is_some())
                || (name == "LOCATION" && change.location.is_some())
                || (name == "DESCRIPTION" && change.notes.is_some())
                || (replaced[3..].contains(&name))
                || (change.moves() && moved.contains(&name)));
        if !drop {
            body.push(line.clone());
        }
    }
    let mut add = vec![format!("DTSTAMP:{}", stamp(now)), format!("LAST-MODIFIED:{}", stamp(now)), format!("SEQUENCE:{}", sequence + 1)];
    if let Some(t) = &change.title {
        add.push(format!("SUMMARY:{}", escape(t)));
    }
    if let Some(l) = change.location.as_deref().filter(|l| !l.trim().is_empty()) {
        add.push(format!("LOCATION:{}", escape(l)));
    }
    if let Some(n) = change.notes.as_deref().filter(|n| !n.trim().is_empty()) {
        add.push(format!("DESCRIPTION:{}", escape(n)));
    }
    if change.moves() {
        add.push(time_line("DTSTART", &start));
        add.push(time_line("DTEND", &end));
    }
    let mut out: Vec<String> = lines[..=b].to_vec();
    out.extend(add);
    out.extend(body);
    out.extend(lines[e..].iter().cloned());
    Ok(out.iter().map(|l| fold(l)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERIES: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VTIMEZONE\r\nTZID:Europe/London\r\nEND:VTIMEZONE\r\nBEGIN:VEVENT\r\nUID:abc\r\nDTSTART;TZID=Europe/London:20261012T090000\r\nDTEND;TZID=Europe/London:20261012T093000\r\nRRULE:FREQ=WEEKLY\r\nSUMMARY:Stand\r\n up\\, daily\r\nSEQUENCE:3\r\nX-APPLE-TRAVEL-ADVISORY-BEHAVIOR:AUTOMATIC\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:Reminder\r\nTRIGGER:-PT10M\r\nEND:VALARM\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:abc\r\nRECURRENCE-ID;TZID=Europe/London:20261019T090000\r\nDTSTART;TZID=Europe/London:20261019T100000\r\nDURATION:PT1H\r\nSUMMARY:Moved\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn reads_events() {
        let evs = events(SERIES);
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].summary, "Standup, daily");
        assert!(evs[0].rrule);
        // 09:00 in London on 12 October (BST) is 08:00 UTC.
        assert_eq!(evs[0].start, Time::At(Utc.with_ymd_and_hms(2026, 10, 12, 8, 0, 0).unwrap()));
        assert_eq!(evs[1].recurrence_id.as_deref(), Some("20261019T090000"));
        assert_eq!(evs[1].end, Time::At(Utc.with_ymd_and_hms(2026, 10, 19, 10, 0, 0).unwrap()));
        // The alarm's DESCRIPTION is not the event's.
        assert_eq!(evs[0].description, None);
    }

    #[test]
    fn all_day_defaults_to_one_day() {
        let e = &events("BEGIN:VEVENT\nUID:x\nDTSTART;VALUE=DATE:20261009\nSUMMARY:Off\nEND:VEVENT\n")[0];
        assert_eq!(e.start, Time::Date(NaiveDate::from_ymd_opt(2026, 10, 9).unwrap()));
        assert_eq!(e.end, Time::Date(NaiveDate::from_ymd_opt(2026, 10, 10).unwrap()));
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("PT1H30M"), Some(5400));
        assert_eq!(parse_duration("P1W"), Some(604_800));
        assert_eq!(parse_duration("-P1DT2H"), Some(-93_600));
        assert_eq!(parse_duration("1H"), None);
    }

    #[test]
    fn escapes_round_trip() {
        let s = "a; b, c\\d\nnext";
        assert_eq!(unescape(&escape(s)), s);
    }

    #[test]
    fn folds_long_lines() {
        let line = format!("SUMMARY:{}", "é".repeat(60));
        let folded = fold(&line);
        assert!(folded.split("\r\n").all(|l| l.len() <= 75));
        assert_eq!(unfold(&folded), vec![line]);
    }

    #[test]
    fn new_event_reads_back() {
        let at = |h| Time::At(Utc.with_ymd_and_hms(2026, 10, 9, h, 0, 0).unwrap());
        let e = NewEvent { calendar_id: "c".into(), title: "Lunch, maybe".into(), start: at(12), end: at(13), location: Some("Café".into()), notes: None };
        let ics = new_event("u1", &e, Utc::now());
        let back = &events(&ics)[0];
        assert_eq!((back.uid.as_str(), back.summary.as_str(), back.location.as_deref()), ("u1", "Lunch, maybe", Some("Café")));
        assert_eq!((back.start, back.end), (at(12), at(13)));
    }

    #[test]
    fn change_keeps_the_rest() {
        let change = EventChange { title: Some("Standup".into()), location: Some("Room 2".into()), ..Default::default() };
        let out = change_event(SERIES, &change, Utc.with_ymd_and_hms(2026, 10, 9, 0, 0, 0).unwrap()).unwrap();
        let evs = events(&out);
        assert_eq!(evs[0].summary, "Standup");
        assert_eq!(evs[0].location.as_deref(), Some("Room 2"));
        assert!(evs[0].rrule);
        assert_eq!(evs[1].summary, "Moved", "the changed occurrence is untouched");
        assert!(out.contains("SEQUENCE:4"));
        assert!(out.contains("TRIGGER:-PT10M") && out.contains("X-APPLE-TRAVEL-ADVISORY-BEHAVIOR"));
        assert!(out.contains("DTSTART;TZID=Europe/London:20261012T090000"));
    }

    #[test]
    fn change_moves_and_clears() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x\r\nDTSTART:20261009T090000Z\r\nDURATION:PT30M\r\nSUMMARY:Call\r\nLOCATION:Zoom\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let start = Time::At(Utc.with_ymd_and_hms(2026, 10, 9, 15, 0, 0).unwrap());
        let change = EventChange { start: Some(start), location: Some(String::new()), ..Default::default() };
        let out = change_event(ics, &change, Utc::now()).unwrap();
        let e = &events(&out)[0];
        assert_eq!(e.start, start);
        assert_eq!(e.end, Time::At(Utc.with_ymd_and_hms(2026, 10, 9, 15, 30, 0).unwrap()));
        assert_eq!(e.location, None);
        assert!(!out.contains("DURATION"));
    }
}
