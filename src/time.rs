use crate::Value;
use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A point in time, to the millisecond, in UTC. Every event in the log has
/// one: the time it was logged.
///
/// Written in the ISO 8601 format `2026-10-24T09:15:02.123Z`, and read back
/// only in exactly that format. Times before 1970 or after year 9999 can't
/// be represented.
///
/// It's also a [`Value`], so entities can hold one. In the binary log format
/// it's written as the number of milliseconds since 1970 instead.
///
/// ```
/// use replayable::Timestamp;
///
/// let t: Timestamp = "2026-10-24T09:15:02.123Z".parse().unwrap();
/// assert_eq!(t.unix_millis(), 1_792_833_302_123);
/// assert_eq!(t.to_string(), "2026-10-24T09:15:02.123Z");
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Timestamp(u64);

/// 9999-12-31T23:59:59.999Z, the last time with a four-digit year.
pub(crate) const MAX_MILLIS: u64 = 253_402_300_799_999;

impl Timestamp {
    /// 1970-01-01T00:00:00.000Z.
    pub const UNIX_EPOCH: Timestamp = Timestamp(0);

    /// The current time from the system clock.
    pub fn now() -> Self {
        SystemTime::now().into()
    }

    /// The time `millis` milliseconds after 1970-01-01T00:00:00.000Z.
    ///
    /// Panics if it's after year 9999.
    pub const fn from_unix_millis(millis: u64) -> Self {
        assert!(millis <= MAX_MILLIS, "a Timestamp can't be after year 9999");
        Timestamp(millis)
    }

    /// Milliseconds since 1970-01-01T00:00:00.000Z.
    pub const fn unix_millis(self) -> u64 {
        self.0
    }
}

/// Rounds down to the millisecond. Times before 1970 become 1970.
///
/// Panics if the time is after year 9999.
impl From<SystemTime> for Timestamp {
    fn from(time: SystemTime) -> Self {
        let millis = time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
        Timestamp::from_unix_millis(u64::try_from(millis).unwrap_or(u64::MAX))
    }
}

impl From<Timestamp> for SystemTime {
    fn from(time: Timestamp) -> Self {
        UNIX_EPOCH + Duration::from_millis(time.0)
    }
}

// Conversion between days since 1970 and dates, from Howard Hinnant's
// `days_from_civil` and `civil_from_days`. Only years from 1970 on are
// needed, so everything stays unsigned.

fn date_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

fn days_from_date(year: u64, month: u64, day: u64) -> u64 {
    let year = year - u64::from(month <= 2);
    let era = year / 400;
    let yoe = year % 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn days_in_month(year: u64, month: u64) -> u64 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl Timestamp {
    /// The ISO 8601 form. Written into a buffer rather than with `write!`
    /// for each part, since every line of the log has one.
    fn to_ascii(self) -> [u8; 24] {
        // Both fit in a u32, whose divisions are cheaper.
        let (days, ms) = ((self.0 / 86_400_000) as u32, (self.0 % 86_400_000) as u32);
        let (year, month, day) = date_from_days(u64::from(days));
        let (year, month, day) = (year as u32, month as u32, day as u32);
        let (s, ms) = (ms / 1000, ms % 1000);
        let mut buf = *b"0000-00-00T00:00:00.000Z";
        let mut two = |at: usize, n: u32| buf[at..at + 2].copy_from_slice(&DIGITS[2 * n as usize..][..2]);
        two(0, year / 100);
        two(2, year % 100);
        two(5, month);
        two(8, day);
        two(11, s / 3600);
        two(14, s / 60 % 60);
        two(17, s % 60);
        two(20, ms / 10);
        buf[22] = b'0' + (ms % 10) as u8;
        buf
    }
}

/// "00", "01", ... "99", for writing two digits at a time.
const DIGITS: &[u8; 200] = b"\
    0001020304050607080910111213141516171819\
    2021222324252627282930313233343536373839\
    4041424344454647484950515253545556575859\
    6061626364656667686970717273747576777879\
    8081828384858687888990919293949596979899";

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(std::str::from_utf8(&self.to_ascii()).expect("only ASCII digits"))
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// The error for a string that isn't a time like `2026-10-24T09:15:02.123Z`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseTimestampError(String);

impl fmt::Display for ParseTimestampError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "`{}` isn't a time like 2026-10-24T09:15:02.123Z (UTC, to the millisecond)", self.0)
    }
}

impl std::error::Error for ParseTimestampError {}

impl FromStr for Timestamp {
    type Err = ParseTimestampError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let error = || ParseTimestampError(s.to_owned());
        let b = s.as_bytes();
        if b.len() != 24 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
            return Err(error());
        }
        if b[19] != b'.' || b[23] != b'Z' {
            return Err(error());
        }
        let number = |at: usize, width: usize| {
            b[at..at + width].iter().try_fold(0, |n, &c| c.is_ascii_digit().then(|| n * 10 + u64::from(c - b'0')))
        };
        let parts = [(0, 4), (5, 2), (8, 2), (11, 2), (14, 2), (17, 2), (20, 3)].map(|(at, w)| number(at, w));
        let [Some(year), Some(month), Some(day), Some(h), Some(m), Some(sec), Some(ms)] = parts else {
            return Err(error());
        };
        if year < 1970 || !(1..=12).contains(&month) || !(1..=days_in_month(year, month)).contains(&day) {
            return Err(error());
        }
        if h > 23 || m > 59 || sec > 59 {
            return Err(error());
        }
        let days = days_from_date(year, month, day);
        Ok(Timestamp(((days * 24 + h) * 60 + m) * 60_000 + sec * 1000 + ms))
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if !s.is_human_readable() {
            return s.serialize_u64(self.0);
        }
        s.serialize_str(std::str::from_utf8(&self.to_ascii()).expect("only ASCII digits"))
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct TimeVisitor;
        impl Visitor<'_> for TimeVisitor {
            type Value = Timestamp;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a time like 2026-10-24T09:15:02.123Z")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Timestamp, E> {
                v.parse().map_err(E::custom)
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Timestamp, E> {
                if v > MAX_MILLIS {
                    return Err(E::custom(format_args!("{v} ms is after year 9999")));
                }
                Ok(Timestamp(v))
            }
        }
        if d.is_human_readable() {
            d.deserialize_str(TimeVisitor)
        } else {
            d.deserialize_u64(TimeVisitor)
        }
    }
}

impl Value for Timestamp {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_times() {
        for (text, millis) in [
            ("1970-01-01T00:00:00.000Z", 0),
            ("1970-01-01T00:00:00.001Z", 1),
            ("2000-02-29T23:59:59.999Z", 951_868_799_999),
            ("2026-10-24T09:15:02.123Z", 1_792_833_302_123),
            ("9999-12-31T23:59:59.999Z", MAX_MILLIS),
        ] {
            assert_eq!(Timestamp(millis).to_string(), text);
            assert_eq!(text.parse(), Ok(Timestamp(millis)), "{text}");
        }
    }

    #[test]
    fn every_day_round_trips() {
        // Every day from 1970 to 2500, at a time that changes with the day.
        for day in 0..days_from_date(2500, 1, 1) {
            let t = Timestamp(day * 86_400_000 + day * 7919 % 86_400_000);
            assert_eq!(t.to_string().parse(), Ok(t));
        }
    }

    #[test]
    fn bad_times_are_rejected() {
        for text in [
            "",
            "2026-10-24",
            "2026-10-24T09:15:02Z",
            "2026-10-24T09:15:02.123",
            "2026-10-24T09:15:02.123+00:00",
            "2026-10-24 09:15:02.123Z",
            "2026-13-01T00:00:00.000Z",
            "2026-02-29T00:00:00.000Z",
            "1900-02-29T00:00:00.000Z",
            "2026-04-31T00:00:00.000Z",
            "2026-10-24T24:00:00.000Z",
            "2026-10-24T09:60:00.000Z",
            "2026-10-24T09:15:60.000Z",
            "1969-12-31T23:59:59.999Z",
            "+026-10-24T09:15:02.123Z",
        ] {
            assert!(text.parse::<Timestamp>().is_err(), "{text}");
        }
        assert!("2024-02-29T00:00:00.000Z".parse::<Timestamp>().is_ok());
    }

    #[test]
    fn system_time_round_trips_to_the_millisecond() {
        let now = SystemTime::now();
        let t = Timestamp::from(now);
        let back = SystemTime::from(t);
        assert!(back <= now && now.duration_since(back).unwrap() < Duration::from_millis(1));
    }
}
