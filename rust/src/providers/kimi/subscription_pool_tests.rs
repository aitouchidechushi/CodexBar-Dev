use super::{KimiCodeApiUsageResponse, KimiProvider};
use serde_json::json;

#[test]
fn new_subscription_has_monthly_pools_without_weekly_usage() {
    let response: KimiCodeApiUsageResponse = serde_json::from_value(json!({
        "usages": {
            "limit_5h": {"used_ratio": 0.125},
            "limit_month_total": {"used_ratio": 1, "reset_time": "2026-11-12T00:00:00Z"},
            "limit_month_code": {"used_ratio": "0.9624", "reset_time": "2026-11-12T00:00:00Z"}
        }
    }))
    .unwrap();
    let usage = KimiProvider::snapshot_from_code_api_response(response).unwrap();
    assert_eq!(usage.primary.window_minutes, Some(300));
    assert_eq!(usage.primary.used_percent, 12.5);
    assert!(usage.secondary.is_none());
    assert_eq!(usage.extra_rate_windows.len(), 2);
    assert_eq!(usage.extra_rate_windows[0].id, "kimi-monthly-total");
    assert_eq!(usage.extra_rate_windows[0].window.used_percent, 100.0);
    let code = &usage.extra_rate_windows[1];
    assert_eq!(code.id, "kimi-monthly-code");
    assert!((code.window.used_percent - 96.24).abs() < 1e-10);
    assert_eq!(
        code.window.resets_at.unwrap().to_rfc3339(),
        "2026-11-12T00:00:00+00:00"
    );
}

#[test]
fn legacy_subscription_still_has_weekly_and_five_hour_quotas() {
    let response = serde_json::from_value(json!({
        "usage": {"limit":"100", "used":"70"},
        "limits": [{"window":{"duration":300,"timeUnit":"TIME_UNIT_MINUTE"},
                    "detail":{"limit":"100","used":"80"}}],
        "usages": {"limit_7d":{"used_ratio":0.702189}, "limit_5h":{"used_ratio":0.843287}}
    }))
    .unwrap();
    let usage = KimiProvider::snapshot_from_code_api_response(response).unwrap();
    assert!((usage.primary.used_percent - 70.2189).abs() < 1e-10);
    assert_eq!(usage.primary.window_minutes, Some(10080));
    assert!((usage.secondary.unwrap().used_percent - 84.3287).abs() < 1e-10);
    assert!(usage.extra_rate_windows.is_empty());
}

#[test]
fn missing_monthly_total_is_not_filled_with_code_quota() {
    let response = serde_json::from_value(json!({"usages": {
        "limit_5h":{"used_ratio":0}, "limit_month_code":{"used_ratio":0.3}
    }}))
    .unwrap();
    let usage = KimiProvider::snapshot_from_code_api_response(response).unwrap();
    assert_eq!(usage.primary.used_percent, 0.0);
    assert_eq!(usage.extra_rate_windows.len(), 1);
    assert_eq!(usage.extra_rate_windows[0].id, "kimi-monthly-code");
}

#[test]
fn empty_response_does_not_imply_available_quota() {
    let response = serde_json::from_value(json!({})).unwrap();
    assert!(KimiProvider::snapshot_from_code_api_response(response).is_err());
}

#[test]
fn legacy_zero_placeholders_use_only_matching_nonzero_counters() {
    let payload = json!({
        "usage": {"limit":"100", "used":"70", "reset_time":"2026-10-20T00:00:00Z"},
        "limits": [{"window":{"duration":300,"timeUnit":"TIME_UNIT_MINUTE"},
                    "detail":{"limit":"100","used":"80","reset_time":"2026-10-13T12:00:00Z"}}],
        "usages": {
            "limit_7d":{"used_ratio":0,"reset_time":"2026-10-20T00:00:01Z"},
            "limit_5h":{"used_ratio":0,"reset_time":"2026-10-13T12:00:01Z"}
        }
    });
    let usage = KimiProvider::snapshot_from_code_api_response(
        serde_json::from_value(payload.clone()).unwrap(),
    )
    .unwrap();
    assert_eq!(usage.primary.used_percent, 70.0);
    assert_eq!(usage.secondary.unwrap().used_percent, 80.0);
    let mut mismatched = payload.clone();
    mismatched["usages"]["limit_5h"]["reset_time"] = json!("2026-10-14T12:00:00Z");
    let usage =
        KimiProvider::snapshot_from_code_api_response(serde_json::from_value(mismatched).unwrap())
            .unwrap();
    assert_eq!(usage.secondary.unwrap().used_percent, 0.0);
    for monthly_pool in ["limit_month_total", "limit_month_code"] {
        for monthly_value in [json!({"used_ratio":0.5}), json!(7)] {
            let mut monthly = payload.clone();
            monthly["usages"][monthly_pool] = monthly_value;
            let usage = KimiProvider::snapshot_from_code_api_response(
                serde_json::from_value(monthly).unwrap(),
            )
            .unwrap();
            assert_eq!(usage.primary.used_percent, 0.0, "{monthly_pool}");
            assert_eq!(usage.secondary.unwrap().used_percent, 0.0, "{monthly_pool}");
        }
    }
}

#[test]
fn malformed_ratio_pools_do_not_discard_other_valid_quotas() {
    let response = serde_json::from_value(json!({"usages": {
        "limit_5h": {"used_ratio":0.2},
        "limit_month_total": {"used_ratio":0.7},
        "limit_month_code": 7
    }}))
    .expect("an invalid Code pool must not discard the other pools");
    let usage = KimiProvider::snapshot_from_code_api_response(response).unwrap();
    assert_eq!(usage.primary.used_percent, 20.0);
    assert_eq!(usage.extra_rate_windows.len(), 1);
    assert_eq!(usage.extra_rate_windows[0].id, "kimi-monthly-total");
    assert_eq!(usage.extra_rate_windows[0].window.used_percent, 70.0);

    for invalid_container in [
        json!([]),
        json!([{"used_ratio":0.2}, null, {"used_ratio":0.7}, null]),
    ] {
        let response = serde_json::from_value(json!({
            "usage": {"limit":"100", "used":"70"}, "usages": invalid_container
        }))
        .expect("an invalid ratio container must not discard legacy quotas");
        let usage = KimiProvider::snapshot_from_code_api_response(response).unwrap();
        assert_eq!(usage.primary.used_percent, 70.0);
        assert_eq!(usage.primary.window_minutes, Some(10080));
        assert!(usage.extra_rate_windows.is_empty());
    }
    let response = serde_json::from_value(json!({"usages": {
        "limit_5h":{"used_ratio":0.2}, "limit_month_total":[0.7,null]
    }}))
    .unwrap();
    let usage = KimiProvider::snapshot_from_code_api_response(response).unwrap();
    assert_eq!(usage.primary.used_percent, 20.0);
    assert!(usage.extra_rate_windows.is_empty());
}

#[test]
fn legacy_zero_placeholder_finds_a_valid_matching_window_after_bad_candidates() {
    for first_detail in [
        json!({"limit":"0", "used":"0", "reset_time":"2026-10-13T12:00:00Z"}),
        json!({"limit":"100", "used":"30", "reset_time":"2026-10-14T12:00:00Z"}),
    ] {
        let response = serde_json::from_value(json!({
            "usage":{"limit":"100","used":"70"},
            "limits":[
                {"window":{"duration":300,"timeUnit":"TIME_UNIT_MINUTE"},"detail":first_detail},
                {"window":{"duration":300,"timeUnit":"TIME_UNIT_MINUTE"},
                 "detail":{"limit":"100","used":"80","reset_time":"2026-10-13T12:00:00Z"}}
            ],
            "usages":{"limit_5h":{"used_ratio":0,"reset_time":"2026-10-13T12:00:01Z"}}
        }))
        .unwrap();
        let usage = KimiProvider::snapshot_from_code_api_response(response).unwrap();
        assert_eq!(usage.secondary.unwrap().used_percent, 80.0);
    }
}
