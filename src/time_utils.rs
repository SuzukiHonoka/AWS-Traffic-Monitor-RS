use aws_smithy_types::DateTime;
use aws_smithy_types_convert::date_time::DateTimeExt;
use chrono::{Datelike, Duration, NaiveDate, TimeZone, Utc};

/// Returns `(start, end, period_seconds)` for the current month, or `None` if the
/// date arithmetic produces an invalid value (should never happen in practice).
pub fn first_and_last_day_of_month(date: NaiveDate) -> Option<(DateTime, DateTime, i64)> {
    let first_day = NaiveDate::from_ymd_opt(date.year(), date.month(), 1)?;
    let first_day_time = first_day.and_hms_opt(0, 0, 0)?;
    let first_day_time_utc = Utc.from_local_datetime(&first_day_time).single()?;
    let fdt = DateTime::from_chrono_utc(first_day_time_utc);

    let next_month_first_day = if date.month() == 12 {
        NaiveDate::from_ymd_opt(date.year() + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(date.year(), date.month() + 1, 1)
    }?;

    let last_day = next_month_first_day - Duration::days(1);
    let last_day_time = last_day.and_hms_opt(23, 59, 59)?;
    let last_day_time_utc = Utc.from_local_datetime(&last_day_time).single()?;
    let ldt = DateTime::from_chrono_utc(last_day_time_utc);

    let period_seconds = (last_day_time - first_day_time).num_seconds() + 1;
    Some((fdt, ldt, period_seconds))
}
