use cairn_plugin_api::Manifest;
use serde_json::json;

#[test]
fn packages_declare_host_managed_oauth_with_scopes_and_nested_token_responses() {
    let value = json!({
        "id": "example", "name": "Example", "version": "1", "apiVersion": 2,
        "capabilities": ["resources", "oauth"], "hosts": ["api.example.test"],
        "settings": [
            {"key": "clientId", "title": "Client ID", "type": "string"},
            {"key": "clientSecret", "title": "Client secret", "type": "secret"}
        ],
        "resources": [{"name": "workspace", "title": "Workspaces", "singular": "Workspace", "settings": [
            {"key": "externalId", "title": "Workspace ID", "type": "string"}
        ]}],
        "oauth": [{
            "name": "bot", "title": "Connect workspace", "collection": "workspace",
            "authorizationUrl": "https://api.example.test/authorize", "tokenUrl": "https://api.example.test/token",
            "clientId": "clientId", "clientSecret": "clientSecret",
            "scopes": ["messages:read", "messages:write"], "scopeSeparator": ",",
            "token": {"accessToken": "/authed_user/access_token", "refreshToken": "/authed_user/refresh_token", "expiresIn": "/authed_user/expires_in", "scope": "/authed_user/scope"},
            "refreshTokenResponse": {"accessToken": "/access_token", "refreshToken": "/refresh_token", "expiresIn": "/expires_in", "scope": "/scope"},
            "metadata": {"externalId": "/team/id"}
        }]
    });
    let manifest: Manifest = serde_json::from_value(value.clone()).unwrap();
    manifest.validate().unwrap();

    let mut overlapping = value.clone();
    overlapping["oauth"][0]["token"]["accessToken"] = json!("/team/id");
    let overlapping: Manifest = serde_json::from_value(overlapping).unwrap();
    assert!(
        overlapping.validate().is_err(),
        "metadata exposed a declared token"
    );

    let mut confidential_pkce = value.clone();
    confidential_pkce["oauth"][0]["pkce"] = json!(true);
    let confidential_pkce: Manifest = serde_json::from_value(confidential_pkce).unwrap();
    confidential_pkce.validate().unwrap();

    let mut other_provider = value.clone();
    other_provider["oauth"][0]["scopeParameter"] = json!("permissions");
    let other_provider: Manifest = serde_json::from_value(other_provider).unwrap();
    other_provider.validate().unwrap();

    let mut personal = value.clone();
    personal["oauth"][0]["personal"] = json!({"subject": "/authed_user/id"});
    let personal_manifest: Manifest = serde_json::from_value(personal.clone()).unwrap();
    personal_manifest.validate().unwrap();
    for subject in [
        "/authed_user/access_token/id",
        "/authed_user/access_token",
        "/profile/name",
    ] {
        let mut invalid = personal.clone();
        invalid["oauth"][0]["personal"]["subject"] = json!(subject);
        let manifest: Manifest = serde_json::from_value(invalid).unwrap();
        assert!(
            manifest.validate().is_err(),
            "accepted unsafe personal subject {subject}"
        );
    }
    personal["oauth"][0]["metadata"] = json!({});
    let manifest: Manifest = serde_json::from_value(personal).unwrap();
    assert!(
        manifest.validate().is_err(),
        "personal credentials lack a resource identity binding"
    );

    for (field, invalid) in [
        ("tokenUrl", json!("https://unapproved.test/token")),
        (
            "authorizationUrl",
            json!("http://api.example.test/authorize"),
        ),
        ("clientSecret", json!("clientId")),
        ("metadata", json!({"externalId": "/access_token"})),
        (
            "metadata",
            json!({"externalId": "/authed_user/access_token/id"}),
        ),
        ("scopes", json!(["messages:read", "messages:read"])),
        ("scopeParameter", json!("state")),
    ] {
        let mut invalid_manifest = value.clone();
        invalid_manifest["oauth"][0][field] = invalid;
        let manifest: Manifest = serde_json::from_value(invalid_manifest).unwrap();
        assert!(manifest.validate().is_err(), "accepted invalid {field}");
    }
}
