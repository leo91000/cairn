use cairn_plugin_api::Manifest;
use serde_json::json;

#[test]
fn packages_declare_related_resource_collections_with_the_same_form_contract() {
    let manifest: Manifest = serde_json::from_value(json!({
        "id": "example", "name": "Example", "version": "1", "apiVersion": 2,
        "capabilities": ["resources"],
        "resources": [
            {"name": "workspace", "title": "Workspaces", "singular": "Workspace", "settings": [
                {"key": "externalId", "title": "Workspace ID", "type": "string", "required": true},
                {"key": "token", "title": "Bot token", "type": "secret"}
            ]},
            {"name": "route", "title": "Routes", "singular": "Route", "parent": "workspace", "delegation": true, "settings": [
                {"key": "channel", "title": "Channel", "type": "string", "required": true}
            ]}
        ]
    })).unwrap();
    manifest.validate().unwrap();
}
