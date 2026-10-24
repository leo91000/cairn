use cairn_plugin_api::Manifest;
use serde_json::json;

#[test]
fn resource_operations_bind_tools_and_commands_to_profiles_requests_and_scopes() {
    let manifest: Manifest = serde_json::from_value(json!({
        "id": "example", "name": "Example", "version": "1", "apiVersion": 2,
        "capabilities": ["resources", "http"], "hosts": ["api.example.test"],
        "agentAccess": {"description": "Choose resources and operations for each agent"},
        "resources": [{"name": "workspace", "title": "Workspaces", "singular": "Workspace", "settings": [
            {"key": "externalId", "title": "External ID", "type": "string", "required": true}
        ]}],
        "operations": [{
            "name": "read-messages", "collection": "workspace", "profile": "read", "scopes": ["channel"],
            "requests": [{"method": "GET", "host": "api.example.test", "path": "/messages", "bindings": {"channel": "channel", "team_id": "resource.externalId"}}]
        }],
        "tools": [{"name": "example_read_messages", "description": "Read messages", "operation": "read-messages",
            "inputSchema": {"type": "object", "required": ["resource", "channel"], "additionalProperties": false,
                "properties": {"resource": {"type": "string"}, "channel": {"type": "string"}}}}],
        "commands": [{"name": "read", "operation": "read-messages", "arguments": [
            {"name": "resource", "flag": "--resource", "required": true},
            {"name": "channel", "flag": "--channel", "required": true}
        ]}]
    })).unwrap();
    manifest.validate().unwrap();
}
