//! Kimi Code API ratio-pool reconciliation (upstream 0.63.0 `fd2414d`,
//! #3755).
//!
//! Mixed legacy responses can carry zero ratio placeholders next to populated
//! counters for the same quota. A zero ratio stays authoritative unless all
//! upstream conditions hold: no monthly ratio pool, reliable legacy weekly
//! counters, a count window of the same duration with nonzero reliable use,
//! and a count reset within two seconds of the ratio reset.

use chrono::TimeDelta;

use super::{
    KimiCodeApiUsageResponse, KimiRatioPool, KimiUsageDetail, RateWindow, format_usage_amount,
};

/// Upstream observed the legacy and ratio reset clocks about 1.45 s apart.
const MATCHING_RESET_TOLERANCE_SECS: i64 = 2;

/// Integer usage counters (upstream `KimiUsageSnapshot.usageCounts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UsageCounts {
    used: i64,
    limit: i64,
    reliable: bool,
}

/// `used` is authoritative and may exceed the limit during overage;
/// `remaining` only counts when it describes a valid balance. A valid limit
/// without usable counters is kept but marked unreliable.
fn usage_counts(detail: &KimiUsageDetail) -> Option<UsageCounts> {
    let limit = integer_counter(detail.limit.as_ref()).filter(|limit| *limit > 0)?;
    if let Some(used) = integer_counter(detail.used.as_ref()).filter(|used| *used >= 0) {
        return Some(UsageCounts {
            used,
            limit,
            reliable: true,
        });
    }
    if let Some(remaining) = integer_counter(detail.remaining.as_ref())
        .filter(|remaining| (0..=limit).contains(remaining))
    {
        return Some(UsageCounts {
            used: limit - remaining,
            limit,
            reliable: true,
        });
    }
    Some(UsageCounts {
        used: 0,
        limit,
        reliable: false,
    })
}

/// Upstream keeps counters as strings and reads them with `Int(_)`: integer
/// strings and integral JSON numbers count; fractions, padded or formatted
/// strings do not.
fn integer_counter(value: Option<&serde_json::Value>) -> Option<i64> {
    match value? {
        serde_json::Value::Number(number) => number.as_i64().or_else(|| {
            let value = number.as_f64()?;
            let integral = value.is_finite()
                && value.fract() == 0.0
                && value >= i64::MIN as f64
                && value < i64::MAX as f64;
            #[allow(
                clippy::cast_possible_truncation,
                reason = "finite integral value is bounded to the i64 range above"
            )]
            integral.then_some(value as i64)
        }),
        serde_json::Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

/// Resolve a ratio pool, replacing a zero placeholder with the matching
/// legacy count window when the response proves the counters are the same
/// quota. `count_window_minutes` is the duration the legacy counters report.
pub(super) fn resolved_ratio_window<'a>(
    response: &KimiCodeApiUsageResponse,
    pool: &KimiRatioPool,
    window_minutes: u32,
    mut details: impl Iterator<Item = (&'a KimiUsageDetail, Option<u32>)>,
) -> Option<RateWindow> {
    let ratio_window = pool.rate_window(window_minutes)?;
    let count_window = details.find_map(|(detail, count_window_minutes)| {
        matching_count_window(
            response,
            &ratio_window,
            Some(detail),
            window_minutes,
            count_window_minutes,
        )
    });
    Some(count_window.unwrap_or(ratio_window))
}

fn matching_count_window(
    response: &KimiCodeApiUsageResponse,
    ratio_window: &RateWindow,
    detail: Option<&KimiUsageDetail>,
    window_minutes: u32,
    count_window_minutes: Option<u32>,
) -> Option<RateWindow> {
    let has_monthly_pool = response
        .usages
        .as_ref()
        .is_some_and(|pools| pools.monthly.is_some() || pools.monthly_code.is_some());
    let weekly_counts_reliable = response
        .usage
        .as_ref()
        .and_then(|value| serde_json::from_value::<KimiUsageDetail>(value.clone()).ok())
        .and_then(|detail| usage_counts(&detail))
        .is_some_and(|counts| counts.reliable);
    if ratio_window.used_percent != 0.0
        || has_monthly_pool
        || !weekly_counts_reliable
        || count_window_minutes != Some(window_minutes)
    {
        return None;
    }

    let detail = detail?;
    let counts = usage_counts(detail).filter(|counts| counts.reliable && counts.used > 0)?;
    let count_reset = detail
        .reset_time
        .as_ref()
        .and_then(super::parse_kimi_timestamp)?;
    let ratio_reset = ratio_window.resets_at?;
    if (count_reset - ratio_reset).abs() > TimeDelta::seconds(MATCHING_RESET_TOLERANCE_SECS) {
        return None;
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "quota counters are far below 2^52; the percent is display-only"
    )]
    let (used, limit) = (counts.used as f64, counts.limit as f64);
    Some(RateWindow::with_details(
        used / limit * 100.0,
        Some(window_minutes),
        Some(count_reset),
        Some(format!(
            "{}/{} credits",
            format_usage_amount(used),
            format_usage_amount(limit)
        )),
    ))
}
