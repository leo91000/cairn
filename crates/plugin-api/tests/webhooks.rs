use cairn_plugin_api::Manifest;
use serde_json::json;

fn signed_webhook() -> Manifest {
    serde_json::from_value(json!({
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
    .unwrap()
}

#[test]
fn packages_declare_signed_public_endpoints_without_exposing_the_signing_secret() {
    signed_webhook().validate().unwrap();
}

#[test]
fn public_endpoint_signatures_cannot_depend_on_private_credential_headers() {
    for header in ["authorization", "cookie", "proxy-authorization"] {
        let mut signature = signed_webhook();
        signature.webhooks[0].signature.header = header.into();
        assert!(
            signature.validate().is_err(),
            "A public endpoint accepted a credential header as its signature: {header}"
        );

        let mut timestamp = signed_webhook();
        timestamp.webhooks[0]
            .signature
            .timestamp
            .as_mut()
            .unwrap()
            .header = header.into();
        assert!(
            timestamp.validate().is_err(),
            "A public endpoint accepted a credential header as its timestamp: {header}"
        );
    }
}
