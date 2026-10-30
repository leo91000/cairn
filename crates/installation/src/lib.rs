pub mod accounts;
pub mod agent_avatars;
pub mod agent_grants;
mod agent_merge;
mod agent_plan;
pub mod api;
pub mod artifacts;
pub mod attachments;
pub mod auth;
pub(crate) mod backend_migration;
pub(crate) mod capabilities;
mod chat_inbox;
pub mod chat_process;
pub mod chat_titles;
pub mod chats;
pub mod claude;
pub mod claude_process;
mod codex_background;
mod codex_login;
pub mod coding_agent;
pub mod config;
pub mod context_usage;
pub mod conversation_deletion;
pub mod conversation_lifecycle;
pub mod delegation;
pub mod direct;
pub mod error;
pub mod execution;
pub mod file_lock;
pub mod http;
mod installation_mcp;
pub mod live;
mod live_text;
pub mod mcp_client;
pub mod mcp_oauth;
pub mod mcp_server;
pub mod mcps;
pub mod microvm;
pub mod missions;
pub mod models;
pub mod native;
pub mod network;
pub mod nodes;
pub mod notifications;
pub mod object_storage;
pub mod opencode;
pub mod outcome;
pub mod performance;
pub mod plugins;
pub mod process;
pub(crate) mod project_archives;
pub mod project_git;
pub mod project_workspaces;
pub mod projects;
pub mod provider;
pub mod reads;
pub mod recovery;
pub mod relay;
pub mod rpc;
mod run_limits;
pub mod run_output;
pub mod run_retry;
pub mod run_status;
pub mod runner;
pub mod runs;
pub mod service;
pub mod skills;
pub mod storage;
pub mod store;
pub mod supervisor;
pub mod tls;
pub mod toolkit;
pub mod validation;
pub mod vault;
pub mod waits;
pub mod worker;

pub mod agents;

#[cfg(test)]
extern crate self as cairn_installation;

#[cfg(test)]
pub(crate) mod test_process;

#[cfg(test)]
#[path = "../tests/common/agent_fixture.rs"]
pub(crate) mod agent_fixture;

#[cfg(test)]
#[path = "../tests/common/mission_fixture.rs"]
pub(crate) mod mission_fixture;

#[cfg(test)]
#[path = "../tests/common/run_fixture.rs"]
pub(crate) mod run_fixture;

#[cfg(test)]
#[path = "../tests/common/node_fixture.rs"]
pub(crate) mod node_fixture;

#[cfg(test)]
#[path = "../tests/common/account_fixture.rs"]
pub(crate) mod account_fixture;
