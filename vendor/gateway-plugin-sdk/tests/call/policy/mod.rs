use gateway_plugin_sdk::{
    SendState,
    call::policy::{
        AccountScheduleCandidate, AccountScheduleDecision, AccountScheduleRequest,
        ModelRouteDecision, ModelRouteRequest, ObserveRequest, PolicyHeader, RequestCost,
        RequestCostSource, RequestCostStatus, RequestFailure, RequestMoney, RequestOutcome,
        RequestTerminal, RequestTimings, RequestUsage,
    },
};
use serde_json::json;

fn observation() -> ObserveRequest {
    ObserveRequest {
        event_id: "request-42:terminal".into(),
        request_id: "request-42".into(),
        config_revision: 7,
        operation: "generate".into(),
        requested_model: Some("public-model".into()),
        client_key_id: Some("key_fixture".into()),
        account_id: Some("acct_fixture".into()),
        upstream_model: Some("upstream-model".into()),
        response_model: Some("reported-model".into()),
        service_tier: Some("default".into()),
        provider: None,
        completed_at_ms: 1234,
        terminal: None,
        usage: None,
    }
}

#[test]
fn terminal_outcomes_have_stable_wire_names() {
    for (outcome, name) in [
        (RequestOutcome::Succeeded, "succeeded"),
        (RequestOutcome::Failed, "failed"),
        (RequestOutcome::Rejected, "rejected"),
        (RequestOutcome::Cancelled, "cancelled"),
        (RequestOutcome::Incomplete, "incomplete"),
    ] {
        let terminal = RequestTerminal {
            outcome,
            send_state: SendState::NotSent,
            attempt_count: 0,
            client_status_code: None,
            error_code: None,
        };
        let wire = serde_json::to_value(&terminal).unwrap();
        assert_eq!(
            wire,
            json!({"outcome": name, "send_state": "not_sent", "attempt_count": 0})
        );
        assert_eq!(
            serde_json::from_value::<RequestTerminal>(wire).unwrap(),
            terminal
        );
    }
}

#[test]
fn observation_exposes_selected_and_reported_facts_without_inventing_values() {
    let mut request = observation();
    let wire = serde_json::to_value(&request).unwrap();
    assert_eq!(wire["client_key_id"], "key_fixture");
    assert_eq!(wire["account_id"], "acct_fixture");
    assert_eq!(wire["upstream_model"], "upstream-model");
    assert_eq!(wire["response_model"], "reported-model");
    assert_eq!(wire["service_tier"], "default");
    request.account_id = None;
    request.upstream_model = None;
    request.response_model = None;
    request.service_tier = None;
    let wire = serde_json::to_value(request).unwrap();
    for field in [
        "account_id",
        "upstream_model",
        "response_model",
        "service_tier",
    ] {
        assert!(wire.get(field).is_none(), "unknown {field} must be omitted");
    }
}

#[test]
fn observation_sections_are_independent_and_omitted_when_not_subscribed() {
    let mut request = observation();
    request.usage = Some(RequestUsage {
        input_tokens: Some(0),
        output_tokens: Some(12),
        ..RequestUsage::default()
    });
    let usage_only = serde_json::to_value(&request).unwrap();
    assert!(usage_only.get("terminal").is_none());
    assert!(usage_only.get("provider").is_none());
    assert_eq!(
        usage_only["usage"],
        json!({"input_tokens": 0, "output_tokens": 12})
    );
    assert_eq!(
        serde_json::from_value::<ObserveRequest>(usage_only).unwrap(),
        request
    );

    request.usage = None;
    request.terminal = Some(RequestTerminal {
        outcome: RequestOutcome::Rejected,
        send_state: SendState::NotSent,
        attempt_count: 0,
        client_status_code: None,
        error_code: Some("invalid_request".into()),
    });
    let lifecycle_only = serde_json::to_value(&request).unwrap();
    assert!(lifecycle_only.get("usage").is_none());
    assert_eq!(
        serde_json::from_value::<ObserveRequest>(lifecycle_only).unwrap(),
        request
    );

    request.provider = Some("example-provider".into());
    request.usage = Some(RequestUsage::default());
    let combined = serde_json::to_value(&request).unwrap();
    assert!(combined.get("terminal").is_some());
    assert_eq!(combined["usage"], json!({}));
    assert_eq!(
        serde_json::from_value::<ObserveRequest>(combined).unwrap(),
        request
    );
}

#[test]
fn missing_usage_is_not_invented_as_zero_or_derived_totals() {
    let usage: RequestUsage = serde_json::from_value(json!({"input_tokens": 0})).unwrap();
    assert_eq!(usage.input_tokens, Some(0));
    assert_eq!(usage.output_tokens, None);
    assert_eq!(usage.total_tokens, None);
    assert_eq!(
        serde_json::to_value(usage).unwrap(),
        json!({"input_tokens": 0})
    );

    assert!(serde_json::from_value::<RequestUsage>(json!({"output_tokens": -1})).is_err());
}

#[test]
fn observation_contract_rejects_unrecognized_fields_and_outcomes() {
    let mut request = serde_json::to_value(observation()).unwrap();
    request["request_body"] = json!({});
    assert!(serde_json::from_value::<ObserveRequest>(request).is_err());
    assert!(serde_json::from_value::<RequestUsage>(json!({"custom_tokens": 1})).is_err());
    assert!(
        serde_json::from_value::<RequestTerminal>(json!({
            "outcome": "succeeded", "send_state": "sent", "attempt_count": 1,
            "internal_error": "not a public observation field"
        }))
        .is_err()
    );
    assert!(serde_json::from_value::<RequestOutcome>(json!("running")).is_err());
}

#[test]
fn cost_timings_and_failure_keep_unknown_facts_explicit_and_safe() {
    let unknown = RequestCost {
        status: RequestCostStatus::Unknown,
        source: RequestCostSource::Unavailable,
        total: None,
    };
    assert_eq!(
        serde_json::to_value(&unknown).unwrap(),
        json!({"status":"unknown","source":"unavailable"})
    );
    let known = RequestCost {
        status: RequestCostStatus::Known,
        source: RequestCostSource::ProviderReported,
        total: Some(RequestMoney {
            amount: "0.0123".into(),
            currency: "USD".into(),
        }),
    };
    assert_eq!(
        serde_json::from_value::<RequestCost>(serde_json::to_value(&known).unwrap()).unwrap(),
        known
    );

    let timings = RequestTimings {
        first_text_ms: Some(12),
        latency_ms: Some(34),
        ..RequestTimings::default()
    };
    assert_eq!(
        serde_json::to_value(&timings).unwrap(),
        json!({"first_text_ms":12,"latency_ms":34})
    );

    let failure = RequestFailure {
        outcome: RequestOutcome::Failed,
        send_state: SendState::Sent,
        attempt_count: 2,
        client_status_code: Some(502),
        upstream_status_code: Some(503),
        error_code: Some("upstream_unavailable".into()),
        retry_after_ms: Some(1_000),
    };
    let wire = serde_json::to_value(&failure).unwrap();
    assert!(wire.get("message").is_none());
    assert!(wire.get("raw_upstream_error").is_none());
    let mut unsafe_wire = wire.clone();
    unsafe_wire["provider_error_code"] = json!("opaque-upstream-value");
    assert!(serde_json::from_value::<RequestFailure>(unsafe_wire).is_err());
    assert_eq!(
        serde_json::from_value::<RequestFailure>(wire).unwrap(),
        failure
    );

    let usage = RequestUsage {
        cost: Some(known),
        timings: Some(timings),
        failure: Some(failure),
        ..RequestUsage::default()
    };
    let wire = serde_json::to_value(&usage).unwrap();
    assert!(wire.get("input_tokens").is_none());
    assert_eq!(wire["cost"]["total"]["amount"], "0.0123");
    assert_eq!(wire["timings"]["latency_ms"], 34);
    assert!(wire["failure"].get("provider_error_code").is_none());
    assert_eq!(serde_json::from_value::<RequestUsage>(wire).unwrap(), usage);
}

#[test]
fn route_and_scheduler_decisions_are_explicit_and_strict() {
    let route = ModelRouteRequest {
        request_id: "req_policy".into(),
        operation: "generate".into(),
        protocol: "openai".into(),
        model: "public-model".into(),
        available_providers: vec!["openai".into()],
        headers: vec![PolicyHeader {
            name: "x-feature".into(),
            value_base64: "b24=".into(),
        }],
    };
    assert_eq!(
        serde_json::from_value::<ModelRouteRequest>(serde_json::to_value(&route).unwrap()).unwrap(),
        route
    );
    assert_eq!(
        serde_json::to_value(ModelRouteDecision::Route {
            provider: Some("openai".into()),
            model: None,
        })
        .unwrap(),
        json!({"decision":"route","provider":"openai"})
    );
    assert!(
        serde_json::from_value::<ModelRouteDecision>(
            json!({"decision":"route","provider":"openai","unexpected":true})
        )
        .is_err()
    );

    let schedule = AccountScheduleRequest {
        request_id: "req_policy".into(),
        attempt_index: 2,
        provider: "openai".into(),
        model: Some("gpt-5".into()),
        candidates: vec![AccountScheduleCandidate {
            account_id: "acct_candidate".into(),
            weight: 10,
            in_flight: 1,
            maximum_concurrency: 4,
            last_started_at_ms: None,
            quota_reset_at_ms: None,
            quota_remaining_rank: Some(9_000),
            failure_rate_basis_points: None,
            first_output_latency_ms: Some(20),
        }],
    };
    assert_eq!(
        serde_json::from_value::<AccountScheduleRequest>(serde_json::to_value(&schedule).unwrap())
            .unwrap(),
        schedule
    );
    assert_eq!(
        serde_json::to_value(AccountScheduleDecision::Pick {
            account_id: "acct_candidate".into(),
        })
        .unwrap(),
        json!({"decision":"pick","account_id":"acct_candidate"})
    );
}
