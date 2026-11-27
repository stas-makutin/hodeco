//! Packaging metadata checks that do not require a package manager.

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct NfpmConfig {
    contents: Vec<NfpmContent>,
}

#[derive(Debug, Deserialize)]
struct NfpmContent {
    #[serde(default)]
    src: Option<String>,
    dst: String,
    #[serde(default, rename = "type")]
    kind: Option<String>,
}

#[test]
fn entities_json_is_installed_as_a_conffile() {
    let config: NfpmConfig =
        serde_yaml_ng::from_str(include_str!("../packaging/nfpm.yaml")).unwrap();

    let entries: Vec<&NfpmContent> = config
        .contents
        .iter()
        .filter(|entry| entry.dst == "/usr/share/hodeco/site/entities.json")
        .collect();

    assert_eq!(
        entries.len(),
        1,
        "entities.json must have exactly one package entry"
    );
    let entry = entries[0];
    assert_eq!(
        entry.kind.as_deref(),
        Some("config|noreplace"),
        "entities.json must survive package upgrades"
    );
    assert!(
        entry
            .src
            .as_deref()
            .is_some_and(|src| src.ends_with("/conffiles/entities.json")),
        "entities.json must be staged outside the replaceable site tree"
    );
}
