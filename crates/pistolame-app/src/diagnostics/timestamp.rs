//! UTC date and time as text, for log lines and crash file names, without a date crate.

use std::time::{SystemTime, UNIX_EPOCH};

const SECONDS_PER_MINUTE: u64 = 60;
const SECONDS_PER_HOUR: u64 = 3_600;
const SECONDS_PER_DAY: u64 = 86_400;

/// A moment in UTC, split into calendar fields. UTC rather than local time: the standard library
/// cannot read the time zone, and UTC lines from different machines compare directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UtcDateTime {
    year: i64,
    month: u32,
    day: u32,
    hour: u64,
    minute: u64,
    second: u64,
}

impl UtcDateTime {
    /// The current moment. A clock set before 1970 reads as 1970-01-01.
    pub(crate) fn now() -> Self {
        let seconds_since_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        Self::from_unix_seconds(seconds_since_epoch)
    }

    fn from_unix_seconds(seconds_since_epoch: u64) -> Self {
        let days_since_epoch = i64::try_from(seconds_since_epoch / SECONDS_PER_DAY).unwrap_or(0);
        let second_of_day = seconds_since_epoch % SECONDS_PER_DAY;
        let (year, month, day) = civil_date_from_days(days_since_epoch);
        Self {
            year,
            month,
            day,
            hour: second_of_day / SECONDS_PER_HOUR,
            minute: second_of_day % SECONDS_PER_HOUR / SECONDS_PER_MINUTE,
            second: second_of_day % SECONDS_PER_MINUTE,
        }
    }

    /// `2026-11-14 10:32:01Z`, the `Z` saying UTC (ISO 8601).
    pub(crate) fn log_text(&self) -> String {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    /// `2026-11-14-103201`: no colon, which Windows forbids in file names; sorts by time.
    pub(crate) fn file_name_text(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}-{:02}{:02}{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// Year, month (1–12) and day (1–31) of the proleptic Gregorian calendar, from days since
/// 1970-01-01. Howard Hinnant's `civil_from_days`:
/// <https://howardhinnant.github.io/date_algorithms.html#civil_from_days>. The constants are his:
/// 719 468 days from 0000-03-01 to 1970-01-01, 146 097 days in a 400-year era, and a year that
/// starts in March so the leap day falls at its end.
fn civil_date_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let days_since_year_zero = days_since_epoch + 719_468;
    let era = days_since_year_zero.div_euclid(146_097);
    let day_of_era = days_since_year_zero.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    // Both fit: the formulas keep the day in 1..=31 and the month in 1..=12.
    (
        year,
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_first_of_january_1970() {
        let moment = UtcDateTime::from_unix_seconds(0);
        assert_eq!(moment.log_text(), "1970-01-01 00:00:00Z");
    }

    #[test]
    fn leap_day_of_2000_is_found() {
        // `date -u -r 951782400` → 2000-02-29 00:00:00.
        let moment = UtcDateTime::from_unix_seconds(951_782_400);
        assert_eq!(moment.log_text(), "2000-02-29 00:00:00Z");
    }

    #[test]
    fn recent_moment_has_hours_minutes_and_seconds() {
        // `date -u -r 1794652321` → 2026-11-14 10:32:01.
        let moment = UtcDateTime::from_unix_seconds(1_794_652_321);
        assert_eq!(moment.log_text(), "2026-11-14 10:32:01Z");
        assert_eq!(moment.file_name_text(), "2026-11-14-103201");
    }

    #[test]
    fn last_second_of_a_year_stays_in_that_year() {
        // `date -u -r 1798761599` → 2026-12-31 23:59:59.
        let moment = UtcDateTime::from_unix_seconds(1_798_761_599);
        assert_eq!(moment.log_text(), "2026-12-31 23:59:59Z");
    }
}
