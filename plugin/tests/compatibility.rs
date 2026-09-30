use gateway_plugin_sdk::Manifest;

fn manifest() -> Manifest {
    Manifest::from_author_slice(include_bytes!("../plugin.json")).unwrap()
}

#[test]
fn supports_verified_manifest_v2_host_series() {
    let requirement = manifest().engines.codex_proxy_rs;
    for host in ["3.18.2", "3.18.3"] {
        assert!(requirement.matches(&host.parse().unwrap()), "host {host}");
    }
}

#[test]
fn excludes_old_protocol_and_unverified_host_series() {
    let requirement = manifest().engines.codex_proxy_rs;
    for host in [
        "3.16.0",
        "3.17.0",
        "3.18.0",
        "3.18.1",
        "3.19.0",
        "4.0.0",
        "3.18.2-rc.1",
    ] {
        assert!(!requirement.matches(&host.parse().unwrap()), "host {host}");
    }
}

#[test]
fn uses_current_host_contracts_without_legacy_capabilities() {
    let value: serde_json::Value =
        serde_json::from_slice(include_bytes!("../plugin.json")).unwrap();
    assert_eq!(value["manifestVersion"], 2);
    assert_eq!(value["contributes"]["upstream_adapter"]["version"], 1);
    assert!(value.get("permissions").is_none());
    assert!(value["contributes"].get("observer").is_some());
    for legacy in [
        "middleware",
        "usage",
        "request_lifecycle",
        "web_socket_observer",
    ] {
        assert!(value["contributes"].get(legacy).is_none());
    }
}
