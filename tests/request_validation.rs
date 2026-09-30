use cpr_excel_companion::prepare;
use serde_json::json;
use std::collections::BTreeMap;

#[test]
fn unsupported_effort_is_not_silently_downgraded() {
    for effort in ["invalid-effort", "", "standard-but-unknown"] {
        let request = json!({"model":"gpt-6-sol","input":"OK","reasoning":{"effort":effort}});
        assert!(
            prepare(&request, &BTreeMap::new()).is_err(),
            "unsupported effort {effort:?} must be rejected"
        );
    }
}

#[test]
fn non_string_effort_is_rejected() {
    for effort in [json!(false), json!(23), json!([]), json!({})] {
        let request = json!({"model":"gpt-6-sol","input":"OK","reasoning":{"effort":effort}});
        assert!(
            prepare(&request, &BTreeMap::new()).is_err(),
            "malformed effort must be rejected"
        );
    }
}

#[test]
fn absent_effort_retains_documented_default() {
    let request = json!({"model":"gpt-6-sol","input":"OK"});
    assert_eq!(
        prepare(&request, &BTreeMap::new()).unwrap()["reasoning_effort"],
        "medium"
    );
}

#[test]
fn supported_effort_is_preserved_and_aliases_are_explicit() {
    for (requested, expected) in [
        ("none", "none"),
        ("low", "low"),
        ("high", "high"),
        ("max", "xhigh"),
    ] {
        let request = json!({"model":"gpt-6-sol","input":"OK","reasoning":{"effort":requested}});
        assert_eq!(
            prepare(&request, &BTreeMap::new()).unwrap()["reasoning_effort"],
            expected
        );
    }
}

#[test]
fn invalid_reasoning_container_is_rejected() {
    for reasoning in [json!("high"), json!([]), json!(42)] {
        let request = json!({"model":"gpt-6-sol","input":"OK","reasoning":reasoning});
        assert!(prepare(&request, &BTreeMap::new()).is_err());
    }
}

#[test]
fn nested_effort_precedes_legacy_field_without_hiding_invalid_values() {
    let invalid = json!({"model":"gpt-6-sol","input":"OK","reasoning":{"effort":false},"reasoning_effort":"high"});
    assert!(prepare(&invalid, &BTreeMap::new()).is_err());
    let valid = json!({"model":"gpt-6-sol","input":"OK","reasoning":{"effort":"low"},"reasoning_effort":"high"});
    assert_eq!(
        prepare(&valid, &BTreeMap::new()).unwrap()["reasoning_effort"],
        "low"
    );
}
