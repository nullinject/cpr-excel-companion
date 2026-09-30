use cpr_excel_companion::{
    admission::{Channel, KeyRule, Overflow, Policy},
    auth::Context,
    control::Control,
};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
struct TestControl {
    path: PathBuf,
    control: Arc<Control>,
}
impl TestControl {
    fn new(policy: Policy) -> Self {
        let path = std::env::temp_dir().join(format!("matrix-{}.json", uuid::Uuid::new_v4()));
        let control = Arc::new(Control::load(path.clone()).unwrap());
        control.save(policy, Some(0)).unwrap();
        Self { path, control }
    }
}
impl Drop for TestControl {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
fn ctx(key: &str, excel: bool) -> Context {
    Context {
        account: "allowed-account".into(),
        scope: key.into(),
        request_id: uuid::Uuid::new_v4().to_string(),
        excel,
        key: None,
        expires: 0,
    }
}
fn policy() -> Policy {
    Policy {
        enabled: true,
        model_channels: BTreeMap::from([
            ("model-a".into(), Channel::Native),
            ("model-b".into(), Channel::Excel),
        ]),
        key_rules: BTreeMap::from([
            (
                "key-a".into(),
                KeyRule {
                    models: BTreeMap::from([
                        ("model-a".into(), Channel::Excel),
                        ("model-b".into(), Channel::Native),
                    ]),
                },
            ),
            (
                "key-b".into(),
                KeyRule {
                    models: BTreeMap::from([
                        ("model-a".into(), Channel::Native),
                        ("model-b".into(), Channel::Excel),
                    ]),
                },
            ),
        ]),
        ..Policy::default()
    }
}
#[tokio::test]
async fn each_key_model_override_wins_over_global_and_client_suffix() {
    let t = TestControl::new(policy());
    for (key, model, expected) in [
        ("key-a", "model-a", true),
        ("key-a", "model-b", false),
        ("key-b", "model-a", false),
        ("key-b", "model-b", true),
    ] {
        for suffix in [false, true] {
            let name = if suffix {
                format!("{model}-excel")
            } else {
                model.to_owned()
            };
            let lease = t.control.enter(&ctx(key, suffix), &name, "-excel").await;
            assert_eq!(lease.excel, expected, "{key} x {name}");
            assert!(!lease.rejected);
        }
    }
}
#[tokio::test]
async fn unknown_keys_and_models_inherit_without_cross_key_leakage() {
    let t = TestControl::new(policy());
    for (key, model, suffix, expected) in [
        ("unknown", "model-a-excel", true, false),
        ("", "model-b", false, true),
        ("key-a", "unlisted-excel", true, true),
        ("key-b", "unlisted", false, false),
    ] {
        let lease = t.control.enter(&ctx(key, suffix), model, "-excel").await;
        assert_eq!(lease.excel, expected);
    }
}
#[tokio::test]
async fn key_overrides_cannot_bypass_channel_account_or_model_limits() {
    for denied in ["disabled", "account", "model"] {
        let mut p = policy();
        match denied {
            "disabled" => p.enabled = false,
            "account" => {
                p.accounts.deny.insert("allowed-account".into());
            }
            _ => {
                p.models.deny.insert("model-a".into());
            }
        }
        let t = TestControl::new(p);
        let lease = t
            .control
            .enter(&ctx("key-a", false), "model-a", "-excel")
            .await;
        assert!(!lease.excel, "{denied}");
    }
}
#[tokio::test]
async fn different_keys_share_excel_capacity_but_native_is_not_throttled() {
    let mut p = policy();
    p.overflow = Overflow::Reject;
    let t = TestControl::new(p);
    let first = t
        .control
        .enter(&ctx("key-a", false), "model-a", "-excel")
        .await;
    assert!(first.excel && !first.rejected);
    let second = t
        .control
        .enter(&ctx("key-b", false), "model-b", "-excel")
        .await;
    assert!(second.excel && second.rejected);
    let native = t
        .control
        .enter(&ctx("key-b", true), "model-a-excel", "-excel")
        .await;
    assert!(!native.excel && !native.rejected);
    drop(first);
    drop(second);
    drop(native);
    let after = t
        .control
        .enter(&ctx("key-a", false), "model-a", "-excel")
        .await;
    assert!(after.excel && !after.rejected);
}
#[test]
fn route_identifiers_reject_ambiguous_whitespace() {
    for (key, model) in [(" key-a", "model-a"), ("key-a", "model-a ")] {
        let mut p = policy();
        p.key_rules.insert(
            key.into(),
            KeyRule {
                models: BTreeMap::from([(model.into(), Channel::Excel)]),
            },
        );
        assert!(p.validate().is_err(), "{key:?} x {model:?}");
    }
}
