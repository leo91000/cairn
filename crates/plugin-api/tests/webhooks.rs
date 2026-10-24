use cairn_plugin_api::Manifest;
use serde_json::json;

#[test]
fn packages_declare_signed_public_endpoints_without_exposing_the_signing_secret() {
    let manifest: Manifest = serde_json::from_value(json!({
        "id": "example", "name": "Example", "version": "1", "apiVersion": 2,
        "capabilities": ["webhooks"],
        "settings": [{ "key": "signingSecret", "title": "Signing secret", "type": "secret" }],
        "webhooks": [{
            "name": "events",
            "signature": {
                "secret": "signingSecret",
                "header": "x-example-signature",
                "prefix": "v0=",
                "template": "v0:{{timestamp}}:{{body}}",
                "timestamp": { "header": "x-example-timestamp", "maxAgeSeconds": 300 }
            }
        }]
    }))
    .unwrap();

    manifest.validate().unwrap();
}
