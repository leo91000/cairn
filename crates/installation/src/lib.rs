pub mod accounts;
pub(crate) mod agent_access;
pub mod agent_avatars;
pub mod agent_grants;
mod agent_merge;
mod agent_plan;
pub mod api;
pub mod artifacts;
pub mod attachments;
pub mod auth;
pub(crate) mod authorization;
pub(crate) mod backend_migration;
pub(crate) mod capabilities;
mod chat_inbox;
pub(crate) mod chat_plan_settings;
pub mod chat_process;
pub mod chat_titles;
pub mod chats;
pub mod claude;
pub mod claude_process;
mod codex_background;
mod codex_login;
pub mod coding_agent;
pub(crate) mod coding_agent_configuration;
pub mod config;
pub mod context_usage;
pub mod conversation_deletion;
pub mod conversation_lifecycle;
pub mod delegation;
pub mod direct;
pub mod environment_expiration;
pub mod error;
pub mod execution;
pub mod file_lock;
pub mod http;
mod installation_mcp;
pub mod live;
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
mod resume_selection;
pub mod rpc;
pub(crate) mod rules;
mod run_limits;
pub mod run_output;
pub mod run_retry;
pub mod run_status;
pub mod runner;
pub mod runs;
pub mod service;
pub mod skills;
pub mod storage;
mod storage_migration;
pub mod store;
pub mod supervisor;
pub(crate) mod task_storage;
pub mod timeline;
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
#[path = "../tests/common/mod.rs"]
pub(crate) mod common;

#[cfg(test)]
#[path = "../tests/fixtures/plugin_grants.rs"]
pub(crate) mod plugin_grant_fixture;

#[cfg(test)]
pub(crate) use common::{
    account_fixture, agent_fixture, attempt_fixture, mission_fixture, node_fixture, relay_fixture,
    run_fixture,
};

#[cfg(test)]
#[path = "../tests/common/placement_migration_fixture.rs"]
pub(crate) mod placement_migration_fixture;
