use super::MCP_FIELDS;
use crate::{
    coding_agent::mcp::{Configuration, Server},
    provider::Provider,
};

#[test]
fn administrator_relays_rewrite_every_coding_agents_mcp_fields() {
    let mut configuration = Configuration::new("token".into());
    configuration.servers.insert(
        "cairn_workspace".into(),
        Server::Http {
            url: format!("{}/mcp-workspace", crate::microvm::mcp::ORIGIN),
            timeout: None,
            enabled_tools: None,
        },
    );
    for provider in Provider::ALL {
        let adapter = provider.adapter();
        let chat = adapter.chat_mcp(&adapter.mcp_configuration(&configuration).unwrap());
        for (field, value) in chat.as_object().unwrap() {
            if value.to_string().contains(crate::microvm::mcp::ORIGIN) {
                assert!(MCP_FIELDS.contains(&field.as_str()), "{provider:?} {field}");
            }
        }
    }
}
