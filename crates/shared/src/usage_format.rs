//! Port of upstream/packages/shared/src/usageFormat.ts
//!
//! Display formatting for the usage page: number, percent, day, and window helpers, plus the
//! time-zone functions (`formatHourShort`, `formatDateTimeShort`, `formatRelativeHourShort`,
//! `makeWindow`). Zones come from `chrono-tz` (IANA database); the viewer's zone comes from the
//! `TZ` variable, then the OS setting, as `Intl.DateTimeFormat().resolvedOptions().timeZone` does.
//! `Intl` output is reproduced for en-US and en-CA only.

use chrono::{DateTime, Datelike, Days, NaiveDate, SecondsFormat, Timelike, Utc};
use chrono_tz::Tz;

use crate::usage_merge::UsageContractMismatchDirection;
use turin_contracts::{
    TrimmedNonEmptyString,
    usage::{UsageDay, UsageResolution, UsageSummaryInput},
};

const HOUR_MS: i64 = 60 * 60 * 1000;

/// `new Intl.NumberFormat("en-US", { style: "currency", currency: "USD", ... }).format(value)`.
pub fn format_usd(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "$∞" } else { "-$∞" }.to_string();
    }
    let fixed = js_to_fixed(value.abs(), 2);
    let (whole, fraction) = fixed.split_once('.').unwrap_or((&fixed, "00"));
    let whole: f64 = whole.parse().unwrap_or(0.0);
    let sign = if value.is_sign_negative() { "-" } else { "" };
    format!("{sign}${}.{fraction}", format_integer(whole))
}

/// `new Intl.NumberFormat("en-US").format(Math.round(value))`.
pub fn format_count(value: f64) -> String {
    format_integer(js_math_round(value))
}

/// `formatPercent(share, digits = 1)`: small positive shares read `<0.1%` instead of `0.0%`.
pub fn format_percent(share: f64, digits: usize) -> String {
    let percent = share * 100.0;
    let smallest = 10_f64.powi(-(digits as i32));
    if percent > 0.0 && percent < smallest {
        return format!("<{}%", js_to_fixed(smallest, digits));
    }
    format!("{}%", js_to_fixed(percent, digits))
}

/// `2026-08-07` to `Aug 7`. Input that does not split into three numbers is returned as is.
pub fn format_day_short(day: &str) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let parts: Vec<Option<i64>> = day.split('-').map(|part| part.parse().ok()).collect();
    match parts.as_slice() {
        [Some(_year), Some(month), Some(day_of_month)] => {
            let name = usize::try_from(*month - 1)
                .ok()
                .and_then(|index| MONTHS.get(index))
                .copied()
                .unwrap_or("");
            format!("{name} {day_of_month}")
        }
        _ => day.to_string(),
    }
}

/// Inclusive day list between two `YYYY-MM-DD` bounds. Empty when either bound does not parse or
/// the range is reversed.
pub fn enumerate_days(since_day: &str, until_day: &str) -> Vec<String> {
    let (Ok(start), Ok(end)) = (
        NaiveDate::parse_from_str(since_day, "%Y-%m-%d"),
        NaiveDate::parse_from_str(until_day, "%Y-%m-%d"),
    ) else {
        return Vec::new();
    };
    let mut days = Vec::new();
    let mut cursor = start;
    while cursor <= end {
        days.push(cursor.format("%Y-%m-%d").to_string());
        match cursor.succ_opt() {
            Some(next) => cursor = next,
            None => break,
        }
    }
    days
}

/// Every fixed-duration bucket start in an hourly rolling window, as ISO instants with millisecond
/// precision.
pub fn enumerate_hour_starts(since_time: &str, until_time: &str) -> Vec<String> {
    let (Ok(start), Ok(end)) = (
        DateTime::parse_from_rfc3339(since_time),
        DateTime::parse_from_rfc3339(until_time),
    ) else {
        return Vec::new();
    };
    let (start, end) = (start.with_timezone(&Utc), end.with_timezone(&Utc));
    let mut starts = Vec::new();
    let mut cursor = start.timestamp_millis();
    let end = end.timestamp_millis();
    while cursor < end {
        if let Some(instant) = DateTime::<Utc>::from_timestamp_millis(cursor) {
            starts.push(instant.to_rfc3339_opts(SecondsFormat::Millis, true));
        }
        cursor += HOUR_MS;
    }
    starts
}

/// Compacts a token count to three significant figures with a unit suffix, so
/// columns of numbers line up at a glance (`19.9B`, `76.7M`, `804K`).
pub fn format_tokens(value: f64) -> String {
    let abs = value.abs();
    if abs >= 1e12 {
        return format!("{}T", trim(value / 1e12));
    }
    if abs >= 1e9 {
        return format!("{}B", trim(value / 1e9));
    }
    if abs >= 1e6 {
        return format!("{}M", trim(value / 1e6));
    }
    if abs >= 1e3 {
        return format!("{}K", trim(value / 1e3));
    }
    format_integer(js_math_round(value))
}

fn trim(value: f64) -> String {
    let abs = value.abs();
    let digits = if abs >= 100.0 {
        0
    } else if abs >= 10.0 {
        1
    } else {
        2
    };
    let fixed = js_to_fixed(value, digits);
    // `.replace(/\.0+$/, "")`: only an all-zero fraction goes, so `1.50` stays.
    match fixed.split_once('.') {
        Some((whole, fraction)) if !fraction.is_empty() && fraction.bytes().all(|b| b == b'0') => {
            whole.to_string()
        }
        _ => fixed,
    }
}

/// JS `Math.round`: halves round towards +infinity (`f64::round` rounds them
/// away from zero), and a result of zero keeps the input's sign (`-0.4` → `-0`).
pub(crate) fn js_math_round(value: f64) -> f64 {
    let floor = value.floor();
    // `value - floor` is the exact fractional part for every finite double.
    let rounded = if value - floor >= 0.5 {
        floor + 1.0
    } else {
        floor
    };
    if rounded == 0.0 {
        0_f64.copysign(value)
    } else {
        rounded
    }
}

/// JS `Number.prototype.toFixed` for the 0–2 digits `trim` asks for.
///
/// Both JS and Rust round the exact binary value, but on an exact tie JS picks
/// the larger magnitude (`(1.125).toFixed(2) === "1.13"`) where Rust's
/// formatter rounds half to even (`"1.12"`). A tie at `digits` decimals is a
/// value whose magnitude times `2^(digits + 1)` is an odd integer, which is
/// exact to test because scaling by a power of two is exact.
fn js_to_fixed(value: f64, digits: usize) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    let abs = value.abs();
    let sign = if value < 0.0 { "-" } else { "" };
    let tie_scale = f64::from(1_u32 << (digits + 1));
    let scaled = abs * tie_scale;
    let is_tie = scaled.fract() == 0.0 && scaled % 2.0 == 1.0 && scaled < 2_f64.powi(53);
    if !is_tie {
        // JS writes the sign for any value below zero, even one that rounds to
        // zero (`(-0.001).toFixed(2)` is "-0.00"), but not for -0 itself.
        return format!("{sign}{abs:.digits$}");
    }
    // abs * 10^digits = scaled * 5^digits / 2; round the half up.
    let five_pow = 5_u64.pow(digits as u32);
    let scaled_int = scaled as u64;
    let rounded = (scaled_int * five_pow).div_ceil(2);
    let ten_pow = 10_u64.pow(digits as u32);
    if digits == 0 {
        format!("{sign}{rounded}")
    } else {
        format!(
            "{sign}{}.{:0width$}",
            rounded / ten_pow,
            rounded % ten_pow,
            width = digits
        )
    }
}

/// `new Intl.NumberFormat("en-US").format(n)` for an already-rounded `n`.
fn format_integer(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "∞" } else { "-∞" }.to_string();
    }
    let digits = format!("{:.0}", value.abs());
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if value.is_sign_negative() {
        grouped.push('-');
    }
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// `formatUsageContractMismatch`: the one-line note for an environment whose usage was excluded.
pub fn format_usage_contract_mismatch(
    environment_label: &str,
    direction: UsageContractMismatchDirection,
) -> String {
    match direction {
        UsageContractMismatchDirection::ServerBehind => {
            format!("{environment_label} runs an older server version and is excluded from totals.")
        }
        UsageContractMismatchDirection::ClientBehind => format!(
            "This client is older than the server on {environment_label}; its usage is excluded from totals."
        ),
    }
}

/// Raised where `Intl.DateTimeFormat` throws a `RangeError` for a zone it does not know.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Invalid time zone specified: {0}")]
pub struct UnknownTimeZone(pub String);

/// The viewer's zone as `Intl` would resolve it: `TZ` when set (a leading `:` is ignored, as
/// libc does), otherwise the OS zone, otherwise UTC.
pub fn system_time_zone() -> String {
    system_time_zone_from(std::env::var("TZ").ok().as_deref())
}

/// [`system_time_zone`] with the `TZ` value passed in, so tests need not touch the environment.
pub fn system_time_zone_from(tz_env: Option<&str>) -> String {
    tz_env
        .map(|value| value.trim_start_matches(':'))
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| iana_time_zone::get_timezone().ok())
        .unwrap_or_else(|| "UTC".to_owned())
}

fn parse_zone(name: &str) -> Result<Tz, UnknownTimeZone> {
    name.parse::<Tz>()
        .map_err(|_| UnknownTimeZone(name.to_owned()))
}

/// `timeZone === undefined` means the system zone, which Intl always accepts; a zone the database
/// lacks falls back to UTC there.
fn zone_for(time_zone: Option<&str>) -> Result<Tz, UnknownTimeZone> {
    match time_zone {
        Some(name) => parse_zone(name),
        None => Ok(parse_zone(&system_time_zone()).unwrap_or(Tz::UTC)),
    }
}

fn parse_instant(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|instant| instant.with_timezone(&Utc))
}

/// `en-US` `{ hour: "numeric" }`: `12 AM`, `8 AM`, `12 PM`.
fn hour_12(hour: u32) -> String {
    let hour12 = if hour.is_multiple_of(12) {
        12
    } else {
        hour % 12
    };
    let period = if hour < 12 { "AM" } else { "PM" };
    format!("{hour12} {period}")
}

/// `en-CA` `{ year, month, day, hour: "2-digit", hourCycle: "h23" }` as a comparable key.
fn wall_hour_key<Z: chrono::TimeZone>(instant: &DateTime<Z>) -> String
where
    Z::Offset: std::fmt::Display,
{
    instant.format("%Y-%m-%d %H").to_string()
}

/// `en-CA` `{ year, month, day }`: `2026-08-11`.
fn day_key<Z: chrono::TimeZone>(instant: &DateTime<Z>) -> String
where
    Z::Offset: std::fmt::Display,
{
    instant.format("%Y-%m-%d").to_string()
}

/// A rolling bucket start rendered in the viewer's requested time zone.
///
/// Repeated wall-clock hours during a fall-back transition include their short zone name so the two
/// distinct buckets remain distinguishable. An unparseable instant is returned unchanged.
pub fn format_hour_short(
    hour_start: &str,
    time_zone: Option<&str>,
) -> Result<String, UnknownTimeZone> {
    let Some(instant) = parse_instant(hour_start) else {
        return Ok(hour_start.to_owned());
    };
    let zone = zone_for(time_zone)?;
    let local = instant.with_timezone(&zone);
    let wall_hour = wall_hour_key(&local);
    let is_repeated_hour = [-HOUR_MS, HOUR_MS].iter().any(|offset| {
        let neighbour = instant + chrono::Duration::milliseconds(*offset);
        wall_hour_key(&neighbour.with_timezone(&zone)) == wall_hour
    });
    if !is_repeated_hour {
        return Ok(hour_12(local.hour()));
    }
    Ok(format!("{} {}", hour_12(local.hour()), local.format("%Z")))
}

/// `2026-08-11T14:37:00Z` to `Aug 11, 2 PM` in the requested zone.
pub fn format_date_time_short(
    instant: &str,
    time_zone: Option<&str>,
) -> Result<String, UnknownTimeZone> {
    let Some(date) = parse_instant(instant) else {
        return Ok(instant.to_owned());
    };
    let local = date.with_timezone(&zone_for(time_zone)?);
    Ok(format!(
        "{} {}, {}",
        MONTH_NAMES[local.month0() as usize],
        local.day(),
        hour_12(local.hour())
    ))
}

/// An hourly tooltip label relative to the rolling window's end date: `2 PM today`, `5 PM yesterday`.
pub fn format_relative_hour_short(
    hour_start: &str,
    relative_to: &str,
    time_zone: Option<&str>,
) -> Result<String, UnknownTimeZone> {
    let (Some(instant), Some(reference)) = (parse_instant(hour_start), parse_instant(relative_to))
    else {
        return format_date_time_short(hour_start, time_zone);
    };
    let zone = zone_for(time_zone)?;
    let instant_day = instant.with_timezone(&zone).date_naive();
    let reference_day = reference.with_timezone(&zone).date_naive();
    let calendar_days_ago = (reference_day - instant_day).num_days();
    let hour = format_hour_short(hour_start, time_zone)?;
    match calendar_days_ago {
        0 => Ok(format!("{hour} today")),
        1 => Ok(format!("{hour} yesterday")),
        _ => format_date_time_short(hour_start, time_zone),
    }
}

const MONTH_NAMES: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// The window the page requests, expressed in the viewer's own time zone so days line up with what
/// they actually experienced.
pub fn make_window(
    days: u64,
    now: DateTime<Utc>,
    resolution: UsageResolution,
) -> UsageSummaryInput {
    make_window_for_zone(&system_time_zone(), days, now, resolution)
}

/// [`make_window`] for an explicit zone. An unknown zone degrades to UTC rather than failing.
pub fn make_window_for_zone(
    time_zone: &str,
    days: u64,
    now: DateTime<Utc>,
    resolution: UsageResolution,
) -> UsageSummaryInput {
    let (zone, time_zone) = match parse_zone(time_zone) {
        Ok(zone) => (zone, time_zone.to_owned()),
        // An unknown zone should degrade to UTC rather than crash the page.
        Err(_) => (Tz::UTC, "UTC".to_owned()),
    };
    let until_day = day_key(&now.with_timezone(&zone));
    let tne =
        |value: String| TrimmedNonEmptyString::new(value).expect("window value is never blank");
    if resolution == UsageResolution::Hour {
        // Minute-aligned bounds keep labels readable while still representing an exact rolling
        // 24-hour duration. Fixed-duration buckets remain correct across offset changes and
        // daylight-saving transitions.
        let until_ms = now.timestamp_millis().div_euclid(60_000) * 60_000;
        let until = DateTime::<Utc>::from_timestamp_millis(until_ms).unwrap_or(now);
        let since = until - chrono::Duration::milliseconds(24 * HOUR_MS);
        return UsageSummaryInput {
            since_day: usage_day(&day_key(&since.with_timezone(&zone))),
            until_day: usage_day(&day_key(&until.with_timezone(&zone))),
            time_zone: tne(time_zone),
            resolution: Some(resolution),
            since_time: Some(tne(since.to_rfc3339_opts(SecondsFormat::Millis, true))),
            until_time: Some(tne(until.to_rfc3339_opts(SecondsFormat::Millis, true))),
            await_refresh: None,
        };
    }
    // Subtracting fixed milliseconds from `now` lands on the wrong calendar day around a DST
    // transition. The window start is pure calendar arithmetic on the local end day.
    let end = NaiveDate::parse_from_str(&until_day, "%Y-%m-%d").unwrap_or_default();
    let start = end
        .checked_sub_days(Days::new(days.saturating_sub(1)))
        .unwrap_or(end);
    UsageSummaryInput {
        since_day: usage_day(&start.format("%Y-%m-%d").to_string()),
        until_day: usage_day(&until_day),
        time_zone: tne(time_zone),
        resolution: Some(resolution),
        since_time: None,
        until_time: None,
        await_refresh: None,
    }
}

fn usage_day(value: &str) -> UsageDay {
    UsageDay::new(value).expect("formatted calendar day is YYYY-MM-DD")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_tokens_matches_upstream() {
        for (value, expected) in [
            (0.0, "0"),
            (999.0, "999"),
            (999.4, "999"),
            (999.5, "1,000"),
            (-0.4, "-0"),
            (-0.5, "-0"),
            (-2.5, "-2"),
            (0.5, "1"),
            (1.5, "2"),
            (1_000.0, "1K"),
            (1_500.0, "1.50K"),
            (12_345.0, "12.3K"),
            (804_000.0, "804K"),
            (999_950.0, "1000K"),
            (1_000_000.0, "1M"),
            (1_125_000.0, "1.13M"),
            (1_234_567.0, "1.23M"),
            (-1_234_567.0, "-1.23M"),
            (1_500_000.0, "1.50M"),
            (76_700_000.0, "76.7M"),
            (19_900_000_000.0, "19.9B"),
            (2.5e15, "2500T"),
            (f64::NAN, "NaN"),
            (f64::INFINITY, "InfinityT"),
            (f64::NEG_INFINITY, "-InfinityT"),
        ] {
            assert_eq!(format_tokens(value), expected, "{value}");
        }
    }

    #[test]
    fn format_percent_distinguishes_a_small_positive_share_from_zero() {
        assert_eq!(format_percent(0.0, 1), "0.0%");
        assert_eq!(format_percent(0.0004, 1), "<0.1%");
        assert_eq!(format_percent(0.0009, 1), "<0.1%");
        assert_eq!(format_percent(0.001, 1), "0.1%");
        assert_eq!(format_percent(0.023, 1), "2.3%");
        assert_eq!(format_percent(0.00004, 2), "<0.01%");
    }

    #[test]
    fn format_day_short_names_the_month_and_day() {
        assert_eq!(format_day_short("2026-08-07"), "Aug 7");
        assert_eq!(format_day_short("not-a-day"), "not-a-day");
    }

    #[test]
    fn enumerate_days_is_inclusive_and_empty_for_reversed_bounds() {
        assert_eq!(
            enumerate_days("2026-02-27", "2026-03-01"),
            vec!["2026-02-27", "2026-02-28", "2026-03-01"]
        );
        assert!(enumerate_days("2026-03-02", "2026-03-01").is_empty());
        assert!(enumerate_days("bad", "2026-03-01").is_empty());
    }

    #[test]
    fn enumerates_24_fixed_hourly_buckets_across_a_rolling_window() {
        let hours = enumerate_hour_starts("2026-08-10T12:37:00.000Z", "2026-08-11T12:37:00.000Z");
        assert_eq!(hours.len(), 24);
        assert_eq!(hours[0], "2026-08-10T12:37:00.000Z");
        assert_eq!(hours[23], "2026-08-11T11:37:00.000Z");
    }

    #[test]
    fn format_usd_and_count_group_thousands() {
        assert_eq!(format_usd(1234.5), "$1,234.50");
        assert_eq!(format_usd(-0.5), "-$0.50");
        assert_eq!(format_usd(-0.001), "-$0.00");
        assert_eq!(format_count(1234.4), "1,234");
    }

    fn utc(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .expect("test instant")
            .with_timezone(&Utc)
    }

    #[test]
    fn hourly_labels_follow_the_requested_zone() {
        let instant = "2026-08-11T12:37:00.000Z";
        assert_eq!(format_hour_short(instant, Some("UTC")).unwrap(), "12 PM");
        assert_eq!(
            format_hour_short(instant, Some("America/New_York")).unwrap(),
            "8 AM"
        );
        assert_eq!(
            format_hour_short(instant, Some("Asia/Kathmandu")).unwrap(),
            "6 PM"
        );
        assert_eq!(format_hour_short(instant, Some("UTC")).unwrap(), "12 PM");
        assert_eq!(
            format_hour_short(instant, Some("Etc/Unknown")),
            Err(UnknownTimeZone("Etc/Unknown".to_owned()))
        );
        assert_eq!(
            format_hour_short("invalid", Some("UTC")).unwrap(),
            "invalid"
        );
    }

    #[test]
    fn system_zone_comes_from_tz_then_the_os() {
        assert_eq!(system_time_zone_from(Some("UTC")), "UTC");
        assert_eq!(
            system_time_zone_from(Some(":America/New_York")),
            "America/New_York"
        );
        let instant = "2026-08-11T12:37:00.000Z";
        let zone = system_time_zone_from(Some("America/New_York"));
        assert_eq!(format_hour_short(instant, Some(&zone)).unwrap(), "8 AM");
        assert_eq!(format_hour_short(instant, Some("UTC")).unwrap(), "12 PM");
    }

    #[test]
    fn rolling_instants_format_in_the_requested_zone() {
        assert_eq!(
            format_hour_short("2026-08-11T00:37:00.000Z", Some("UTC")).unwrap(),
            "12 AM"
        );
        assert_eq!(
            format_hour_short("2026-08-11T12:37:00.000Z", Some("UTC")).unwrap(),
            "12 PM"
        );
        assert_eq!(
            format_date_time_short("2026-08-11T17:37:00.000Z", Some("UTC")).unwrap(),
            "Aug 11, 5 PM"
        );
    }

    #[test]
    fn repeated_hours_during_fall_back_carry_the_zone_name() {
        assert_eq!(
            format_hour_short("2026-11-01T05:37:00.000Z", Some("America/New_York")).unwrap(),
            "1 AM EDT"
        );
        assert_eq!(
            format_hour_short("2026-11-01T06:37:00.000Z", Some("America/New_York")).unwrap(),
            "1 AM EST"
        );
    }

    #[test]
    fn tooltip_dates_are_relative_to_the_window_end_in_the_requested_zone() {
        let window_end = "2026-08-11T14:37:00.000Z";
        assert_eq!(
            format_relative_hour_short("2026-08-10T17:37:00.000Z", window_end, Some("UTC"))
                .unwrap(),
            "5 PM yesterday"
        );
        assert_eq!(
            format_relative_hour_short("2026-08-11T14:37:00.000Z", window_end, Some("UTC"))
                .unwrap(),
            "2 PM today"
        );
        assert_eq!(
            format_relative_hour_short(
                "2026-08-11T01:37:00.000Z",
                "2026-08-11T10:37:00.000Z",
                Some("America/Los_Angeles"),
            )
            .unwrap(),
            "6 PM yesterday"
        );
    }

    #[test]
    fn hour_window_is_an_exact_minute_aligned_24_hour_request() {
        let window = make_window(1, utc("2026-08-11T12:37:42.123Z"), UsageResolution::Hour);
        assert_eq!(window.resolution, Some(UsageResolution::Hour));
        assert_eq!(
            window.since_time.as_ref().map(|t| t.as_str()),
            Some("2026-08-10T12:37:00.000Z")
        );
        assert_eq!(
            window.until_time.as_ref().map(|t| t.as_str()),
            Some("2026-08-11T12:37:00.000Z")
        );
    }

    #[test]
    fn unknown_zone_degrades_to_utc_instead_of_failing() {
        let now = utc("2026-08-11T12:37:42.123Z");
        assert_eq!(
            make_window_for_zone("Etc/Unknown", 1, now, UsageResolution::Hour)
                .time_zone
                .as_str(),
            "UTC"
        );
        assert_eq!(
            make_window_for_zone("Etc/Unknown", 30, now, UsageResolution::Day)
                .time_zone
                .as_str(),
            "UTC"
        );
    }

    #[test]
    fn day_window_counts_calendar_days_in_the_zone() {
        let window = make_window_for_zone(
            "America/New_York",
            3,
            utc("2026-08-11T02:30:00.000Z"),
            UsageResolution::Day,
        );
        // 02:30Z is still Aug 10 in New York, so a three-day window ends there and starts Aug 8.
        assert_eq!(window.until_day.as_str(), "2026-08-10");
        assert_eq!(window.since_day.as_str(), "2026-08-08");
        assert!(window.since_time.is_none());
    }

    #[test]
    fn contract_mismatch_names_the_side_that_needs_updating() {
        assert_eq!(
            format_usage_contract_mismatch("Desktop", UsageContractMismatchDirection::ServerBehind),
            "Desktop runs an older server version and is excluded from totals."
        );
        assert_eq!(
            format_usage_contract_mismatch("Desktop", UsageContractMismatchDirection::ClientBehind),
            "This client is older than the server on Desktop; its usage is excluded from totals."
        );
    }
}
