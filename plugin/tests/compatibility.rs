use gateway_plugin_sdk::Manifest;

fn manifest() -> Manifest {
    Manifest::from_author_slice(include_bytes!("../plugin.json")).unwrap()
}

#[test]
fn supports_stable_host_updates_within_major_three() {
    let requirement = manifest().engines.codex_proxy_rs;
    for host in ["3.16.0", "3.16.9", "3.17.0", "3.18.0", "3.99.0"] {
        assert!(requirement.matches(&host.parse().unwrap()), "host {host}");
    }
}

#[test]
fn excludes_older_major_four_and_prerelease_hosts() {
    let requirement = manifest().engines.codex_proxy_rs;
    for host in ["3.15.9", "4.0.0", "4.1.0", "3.17.0-rc.1"] {
        assert!(!requirement.matches(&host.parse().unwrap()), "host {host}");
    }
}
