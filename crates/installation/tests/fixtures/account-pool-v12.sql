-- Real SQLite v12 Store/Account/Vault fixture from 4af96bbe; synthetic data only.
-- A retained interrupted run/checkpoint is seeded with the version-12 SQL contract.
PRAGMA foreign_keys=OFF;
BEGIN TRANSACTION;
CREATE TABLE agent_exclusive_nodes(
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 node_id TEXT NOT NULL CHECK(length(node_id)=36),
 position INTEGER NOT NULL CHECK(position>=0),
 PRIMARY KEY(agent_id,node_id), UNIQUE(agent_id,position)
) WITHOUT ROWID;
CREATE TABLE agent_mcp_selections(
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 mcp_id TEXT NOT NULL REFERENCES mcp_servers(id) ON DELETE RESTRICT CHECK(length(mcp_id)=36),
 PRIMARY KEY(agent_id,mcp_id)
) WITHOUT ROWID;
CREATE TABLE agent_mcp_tools(
 agent_id TEXT NOT NULL,
 mcp_id TEXT NOT NULL REFERENCES mcp_servers(id) ON DELETE RESTRICT,
 tool TEXT NOT NULL CHECK(length(tool) BETWEEN 1 AND 200),
 position INTEGER NOT NULL CHECK(position>=0),
 PRIMARY KEY(agent_id,mcp_id,tool), UNIQUE(agent_id,mcp_id,position),
 FOREIGN KEY(agent_id,mcp_id) REFERENCES agent_mcp_selections(agent_id,mcp_id) ON DELETE CASCADE
) WITHOUT ROWID;
CREATE TABLE agent_mcps(
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 mcp_id TEXT NOT NULL REFERENCES mcp_servers(id) ON DELETE RESTRICT CHECK(length(mcp_id)=36),
 position INTEGER NOT NULL CHECK(position>=0),
 PRIMARY KEY(agent_id,mcp_id), UNIQUE(agent_id,position)
) WITHOUT ROWID;
CREATE TABLE agent_node_priorities(
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 node_id TEXT NOT NULL,
 position INTEGER NOT NULL CHECK(position>=0),
 PRIMARY KEY(agent_id,node_id), UNIQUE(agent_id,position)
) WITHOUT ROWID;
CREATE TABLE agent_nodes(
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 node_id TEXT NOT NULL CHECK(length(node_id)=36),
 position INTEGER NOT NULL CHECK(position>=0),
 PRIMARY KEY(agent_id,node_id), UNIQUE(agent_id,position)
) WITHOUT ROWID;
CREATE TABLE agent_permissions(
 agent_id TEXT PRIMARY KEY NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 backend TEXT NOT NULL CHECK(backend IN ('auto','firecracker','native')),
 sandbox TEXT NOT NULL CHECK(sandbox IN ('yolo','workspace-write','read-only')),
 installation INTEGER NOT NULL CHECK(installation IN (0,1)),
 delegation INTEGER NOT NULL CHECK(delegation IN (0,1)),
 all_projects INTEGER NOT NULL CHECK(all_projects IN (0,1)),
 all_skills INTEGER NOT NULL CHECK(all_skills IN (0,1)),
 all_nodes INTEGER NOT NULL CHECK(all_nodes IN (0,1)),
 all_mcps INTEGER NOT NULL CHECK(all_mcps IN (0,1))
) WITHOUT ROWID;
CREATE TABLE agent_projects(
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
 position INTEGER NOT NULL CHECK(position>=0),
 PRIMARY KEY(agent_id,project_id), UNIQUE(agent_id,position)
) WITHOUT ROWID;
CREATE TABLE agent_skills(
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 skill TEXT NOT NULL CHECK(length(skill)<=160),
 position INTEGER NOT NULL CHECK(position>=0),
 PRIMARY KEY(agent_id,skill), UNIQUE(agent_id,position)
) WITHOUT ROWID;
CREATE TABLE agents(
 id TEXT PRIMARY KEY NOT NULL,
 name TEXT NOT NULL CHECK(length(trim(name)) BETWEEN 1 AND 100),
 provider TEXT NOT NULL CHECK(provider IN ('codex','claude','opencode')),
 description TEXT NOT NULL CHECK(length(description)<=500),
 model TEXT NOT NULL CHECK(length(model)<=100),
 reasoning TEXT NOT NULL CHECK(length(reasoning)<=40),
 speed TEXT NOT NULL CHECK(length(speed)<=40),
 instructions TEXT NOT NULL CHECK(length(instructions)<=20000),
 timeout_minutes INTEGER NOT NULL CHECK(timeout_minutes BETWEEN 0 AND 720),
 revision TEXT NOT NULL CHECK(length(revision)>0),
 created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
 updated_at INTEGER NOT NULL,
 avatar_status TEXT CHECK(avatar_status IN ('generating','ready','failed')),
 avatar_revision TEXT,
 avatar_url TEXT,
 avatar_error TEXT,
 CHECK((avatar_status IS NULL AND avatar_revision IS NULL AND avatar_url IS NULL AND avatar_error IS NULL) OR (avatar_status IS NOT NULL AND avatar_revision IS NOT NULL))
);
CREATE TABLE audit(id INTEGER PRIMARY KEY AUTOINCREMENT,created_at INTEGER NOT NULL,action TEXT NOT NULL,detail TEXT NOT NULL);
CREATE TABLE chat_messages(id TEXT PRIMARY KEY,chat_id TEXT NOT NULL,data TEXT NOT NULL,created_at INTEGER NOT NULL);
CREATE TABLE events(id INTEGER PRIMARY KEY AUTOINCREMENT,run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),type TEXT NOT NULL CHECK(length(type)>0),text TEXT NOT NULL,payload TEXT CHECK(payload IS NULL OR json_valid(payload)));
CREATE TABLE external_delegations(
 instance_id TEXT PRIMARY KEY NOT NULL REFERENCES plugin_instances(id) ON DELETE CASCADE,
 agent_id TEXT NOT NULL REFERENCES agents(id),
 project_id TEXT REFERENCES projects(id),
 granted_access TEXT NOT NULL CHECK(json_valid(granted_access) AND json_type(granted_access)='object'),
 native_authority TEXT CHECK(native_authority IS NULL OR (json_valid(native_authority) AND json_type(native_authority)='object')),
 access_mode TEXT NOT NULL CHECK(access_mode IN ('allowlist','requests','everyone')),
 max_messages_per_hour INTEGER NOT NULL CHECK(max_messages_per_hour BETWEEN 1 AND 100),
 max_per_person_per_hour INTEGER NOT NULL CHECK(max_per_person_per_hour BETWEEN 1 AND 50)
) WITHOUT ROWID;
CREATE TABLE instance_grants(
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 instance_id TEXT NOT NULL REFERENCES plugin_instances(id) CHECK(length(instance_id)=36),
 position INTEGER NOT NULL CHECK(position>=0),
 PRIMARY KEY(agent_id,instance_id), UNIQUE(agent_id,position)
) WITHOUT ROWID;
CREATE TABLE instance_people(
 instance_id TEXT NOT NULL REFERENCES plugin_instances(id) ON DELETE CASCADE,
 user_id TEXT NOT NULL CHECK(length(user_id) BETWEEN 1 AND 64),
 name TEXT NOT NULL CHECK(length(name)<=100),
 position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 99),
 PRIMARY KEY(instance_id,user_id), UNIQUE(instance_id,position)
) WITHOUT ROWID;
CREATE TABLE kv(key TEXT PRIMARY KEY CHECK(key NOT LIKE 'run-checkpoint:%'),data TEXT NOT NULL,expires INTEGER);
INSERT INTO "kv" VALUES('mcp-secret:synthetic:00000000-0000-4000-8000-000000000001','"Hv8FfBA7+0hq/FGjlRSiVOHO/eODsBNrUe91SqPb0A+r7gm7qOGLY4xPq7dCkYrJjvaUMST3TKlXXJ60SrB2hCs="',NULL);
INSERT INTO "kv" VALUES('mcp-secret:synthetic:00000000-0000-4000-8000-000000000002','"Al553tpQZAdZsNT3EUBEESEKlOXGZoYQD8nVw+mX3l3T7t6Az9CIruZ4QjDpAWwY0jK49Y8if6KOT+bkhlSSIjE="',NULL);
INSERT INTO "kv" VALUES('mcp-secret:synthetic:00000000-0000-4000-8000-000000000003','"7JKXWaryh3L6Qo6gcu1lEPv3yZ0z6h+aB3gBKECWr9ElMRVmI9GACUdlghcji8B4sFHipFB3JgFBR8Qu/5sxXRQ="',NULL);
INSERT INTO "kv" VALUES('synthetic-expiry','{"counter":9007199254740001}',9999999999999);
CREATE TABLE mcp_args(server_id TEXT NOT NULL REFERENCES mcp_servers(id) ON DELETE CASCADE,position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 99),value TEXT NOT NULL CHECK(length(value)<=4000),PRIMARY KEY(server_id,position)) WITHOUT ROWID;
CREATE TABLE mcp_credentials(server_id TEXT PRIMARY KEY NOT NULL REFERENCES mcp_servers(id) ON DELETE CASCADE,ciphertext BLOB NOT NULL CHECK(length(ciphertext)>=28)) WITHOUT ROWID;
CREATE TABLE mcp_discovery_cache(server_id TEXT PRIMARY KEY NOT NULL REFERENCES mcp_servers(id) ON DELETE CASCADE,tools TEXT NOT NULL CHECK(json_valid(tools) AND json_type(tools)='array'));
CREATE TABLE mcp_enabled_tools(server_id TEXT NOT NULL REFERENCES mcp_servers(id) ON DELETE CASCADE,position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 499),name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 200),PRIMARY KEY(server_id,position)) WITHOUT ROWID;
CREATE TABLE mcp_grant_servers(digest TEXT NOT NULL REFERENCES mcp_run_grants(digest) ON DELETE CASCADE,server_id TEXT NOT NULL REFERENCES mcp_servers(id) ON DELETE CASCADE,revision INTEGER NOT NULL CHECK(revision>=0),all_tools INTEGER NOT NULL CHECK(all_tools IN (0,1)),PRIMARY KEY(digest,server_id)) WITHOUT ROWID;
CREATE TABLE mcp_grant_tools(digest TEXT NOT NULL,server_id TEXT NOT NULL,position INTEGER NOT NULL CHECK(position>=0),name TEXT NOT NULL,PRIMARY KEY(digest,server_id,position),FOREIGN KEY(digest,server_id) REFERENCES mcp_grant_servers(digest,server_id) ON DELETE CASCADE) WITHOUT ROWID;
CREATE TABLE mcp_pending_oauth(digest TEXT PRIMARY KEY NOT NULL,server_id TEXT NOT NULL REFERENCES mcp_servers(id) ON DELETE CASCADE,revision INTEGER NOT NULL CHECK(revision>=0),session TEXT NOT NULL,nonce TEXT NOT NULL,native INTEGER NOT NULL CHECK(native IN (0,1)),expires_at INTEGER NOT NULL,callback BLOB CHECK(callback IS NULL OR length(callback)>=28)) WITHOUT ROWID;
CREATE TABLE mcp_run_grants(digest TEXT PRIMARY KEY NOT NULL,run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,message_id TEXT,workspace INTEGER NOT NULL CHECK(workspace IN (0,1)),installation INTEGER NOT NULL CHECK(installation IN (0,1)),expires_at INTEGER) WITHOUT ROWID;
CREATE TABLE mcp_scopes(server_id TEXT NOT NULL REFERENCES mcp_servers(id) ON DELETE CASCADE,position INTEGER NOT NULL CHECK(position>=0),value TEXT NOT NULL CHECK(length(value)>0),PRIMARY KEY(server_id,position)) WITHOUT ROWID;
CREATE TABLE mcp_servers(
 id TEXT PRIMARY KEY NOT NULL,
 name TEXT NOT NULL CHECK(length(trim(name)) BETWEEN 1 AND 100),
 transport TEXT NOT NULL CHECK(transport IN ('http','stdio')),
 url TEXT NOT NULL CHECK(length(url)<=2000),
 command TEXT NOT NULL CHECK(length(command)<=1000),
 auth TEXT NOT NULL CHECK(auth IN ('none','bearer','oauth')),
 client_id TEXT NOT NULL CHECK(length(client_id)<=500),
 allow_private_network INTEGER NOT NULL CHECK(allow_private_network IN (0,1)),
 enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),
 all_tools INTEGER NOT NULL CHECK(all_tools IN (0,1)),
 created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
 revision INTEGER NOT NULL CHECK(revision BETWEEN 0 AND 9007199254740991),
 state TEXT NOT NULL CHECK(state IN ('untested','connected','needs-auth','error')),
 checked_at INTEGER CHECK(checked_at BETWEEN 0 AND 9007199254740991),
 error TEXT NOT NULL,
 updated_at INTEGER NOT NULL,
 CHECK(transport!='stdio' OR (length(trim(command))>0 AND auth='none'))
);
CREATE TABLE mission_skills(
 mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
 position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 99),
 skill TEXT NOT NULL CHECK(length(skill)<=160),
 PRIMARY KEY(mission_id,position)
) WITHOUT ROWID;
CREATE TABLE mission_tags(
 mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
 position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 9),
 tag TEXT NOT NULL CHECK(length(tag)<=30),
 PRIMARY KEY(mission_id,position)
) WITHOUT ROWID;
CREATE TABLE missions(
 id TEXT PRIMARY KEY NOT NULL,
 agent_id TEXT NOT NULL REFERENCES agents(id),
 project_id TEXT REFERENCES projects(id),
 name TEXT NOT NULL CHECK(length(trim(name)) BETWEEN 1 AND 100),
 prompt TEXT NOT NULL CHECK(length(trim(prompt)) BETWEEN 1 AND 50000),
 cron TEXT CHECK(cron IS NULL OR length(cron)<=100),
 timezone TEXT NOT NULL CHECK(length(timezone) BETWEEN 1 AND 100),
 enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),
 archived INTEGER NOT NULL CHECK(archived IN (0,1)),
 worktree INTEGER NOT NULL CHECK(worktree IN (0,1)),
 skills_inherited INTEGER NOT NULL CHECK(skills_inherited IN (0,1)),
 next_run INTEGER CHECK(next_run BETWEEN 0 AND 9007199254740991),
 author_id TEXT,
 author_access_id TEXT,
 author_removed INTEGER NOT NULL CHECK(author_removed IN (0,1)),
 revision TEXT NOT NULL CHECK(length(revision)>0),
 created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
 updated_at INTEGER NOT NULL,
 schedule_wait_reason TEXT,
 failure_message TEXT,
 failure_scheduled_for INTEGER CHECK(failure_scheduled_for BETWEEN 0 AND 9007199254740991),
 failure_at INTEGER CHECK(failure_at BETWEEN 0 AND 9007199254740991),
 native_principals TEXT NOT NULL CHECK(json_valid(native_principals) AND json_type(native_principals)='array'),
 native_authority_nodes TEXT NOT NULL CHECK(json_valid(native_authority_nodes) AND json_type(native_authority_nodes)='array'),
 native_parent_run_id TEXT,
 native_parent_access TEXT CHECK(native_parent_access IN ('installation','delegation')),
 native_provenance_unknown INTEGER NOT NULL CHECK(native_provenance_unknown IN (0,1)),
 native_plugin_grants TEXT CHECK(native_plugin_grants IS NULL OR (json_valid(native_plugin_grants) AND json_type(native_plugin_grants)='array')),
 CHECK((author_id IS NULL)=(author_access_id IS NULL)),
 CHECK(next_run IS NULL OR (enabled=1 AND archived=0 AND cron IS NOT NULL AND author_removed=0)),
 CHECK((failure_message IS NULL AND failure_scheduled_for IS NULL AND failure_at IS NULL)
    OR (failure_message IS NOT NULL AND failure_scheduled_for IS NOT NULL AND failure_at IS NOT NULL))
);
CREATE TABLE node_reviewed_agents(
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK(position>=0),
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    PRIMARY KEY(node_id,position)
) WITHOUT ROWID;
CREATE TABLE node_tags(
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    system INTEGER NOT NULL CHECK(system IN (0,1)),
    position INTEGER NOT NULL CHECK(position>=0),
    tag TEXT NOT NULL CHECK(length(tag) BETWEEN 1 AND 40 AND tag NOT GLOB '*[^A-Za-z0-9_:./-]*'),
    PRIMARY KEY(node_id,system,position)
) WITHOUT ROWID;
CREATE TABLE node_trusted_accounts(
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 99),
    account_id TEXT NOT NULL CHECK(length(account_id) BETWEEN 1 AND 200),
    PRIMARY KEY(node_id,position)
) WITHOUT ROWID;
CREATE TABLE nodes(
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL CHECK(length(trim(name)) BETWEEN 1 AND 100),
    revision TEXT NOT NULL CHECK(length(revision)>0),
    local INTEGER NOT NULL CHECK(local IN (0,1)),
    accepting INTEGER NOT NULL CHECK(accepting IN (0,1)),
    revoked INTEGER NOT NULL CHECK(revoked IN (0,1)),
    cap_os TEXT NOT NULL,
    cap_arch TEXT NOT NULL,
    backend TEXT NOT NULL CHECK(backend IN ('firecracker','native')),
    kvm INTEGER NOT NULL CHECK(kvm IN (0,1)),
    ublk INTEGER NOT NULL CHECK(ublk IN (0,1)),
    native_mode_protocol INTEGER NOT NULL,
    capacity_cpu INTEGER NOT NULL CHECK(capacity_cpu BETWEEN 0 AND 1099511627776),
    capacity_memory_mi_b INTEGER NOT NULL CHECK(capacity_memory_mi_b BETWEEN 0 AND 1099511627776),
    capacity_disk_mi_b INTEGER NOT NULL CHECK(capacity_disk_mi_b BETWEEN 0 AND 1099511627776),
    capacity_disk_total_mi_b INTEGER NOT NULL CHECK(capacity_disk_total_mi_b BETWEEN 0 AND 1099511627776),
    limit_cpu INTEGER NOT NULL CHECK(limit_cpu BETWEEN 1 AND 4096),
    limit_memory_mi_b INTEGER NOT NULL CHECK(limit_memory_mi_b BETWEEN 128 AND 1073741824),
    limit_disk_mi_b INTEGER NOT NULL CHECK(limit_disk_mi_b BETWEEN 128 AND 1099511627776),
    slots INTEGER CHECK(slots IS NULL OR slots BETWEEN 1 AND 4096),
    runtime_slots INTEGER CHECK(runtime_slots IS NULL OR runtime_slots BETWEEN 1 AND 4096),
    native_enabled INTEGER NOT NULL CHECK(native_enabled IN (0,1)),
    native_mode_request TEXT CHECK(native_mode_request IS NULL OR json_valid(native_mode_request)),
    native_mode_observed TEXT CHECK(native_mode_observed IS NULL OR json_valid(native_mode_observed)),
    native_mode_proposal TEXT CHECK(native_mode_proposal IS NULL OR json_valid(native_mode_proposal)),
    native_mode_status TEXT CHECK(native_mode_status IS NULL OR native_mode_status IN ('pending','applied','failed')),
    exclusive_reservations INTEGER NOT NULL CHECK(exclusive_reservations IN (0,1)),
    exclusive_reservation TEXT CHECK(exclusive_reservation IS NULL OR json_valid(exclusive_reservation)),
    usage TEXT CHECK(usage IS NULL OR json_valid(usage)),
    budget_error TEXT ,
    pressure TEXT ,
    execution_ready INTEGER NOT NULL CHECK(execution_ready IN (0,1)),
    maintenance TEXT ,
    maintenance_error TEXT ,
    maintenance_generation TEXT ,
    maintenance_started_at INTEGER CHECK(maintenance_started_at IS NULL OR maintenance_started_at BETWEEN 0 AND 9007199254740991),
    image_digest TEXT ,
    update_error TEXT ,
    runtime_id TEXT NOT NULL,
    last_seen INTEGER CHECK(last_seen IS NULL OR last_seen BETWEEN 0 AND 9007199254740991),
    created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
    storage_generation INTEGER NOT NULL DEFAULT 0 CHECK(storage_generation BETWEEN 0 AND 9007199254740991),
    storage_cache_mi_b INTEGER NOT NULL,
    storage_memory_cache_mi_b INTEGER NOT NULL,
    storage_reserve_mi_b INTEGER NOT NULL,
    storage_reserve_percent INTEGER NOT NULL,
    storage_backup_seconds INTEGER NOT NULL,
    storage_max_dirty_seconds INTEGER NOT NULL,
    disk_growth_protocol INTEGER ,
    retained_runs TEXT CHECK(retained_runs IS NULL OR json_valid(retained_runs)),
    disk_capacity TEXT CHECK(disk_capacity IS NULL OR json_valid(disk_capacity)),
    updated_at INTEGER NOT NULL
);
CREATE TABLE plugin_activity(
 id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=36),
 delivery_id TEXT NOT NULL,
 instance_id TEXT NOT NULL REFERENCES plugin_instances(id) ON DELETE CASCADE,
 chat_id TEXT NOT NULL,
 user_id TEXT NOT NULL,
 name TEXT NOT NULL,
 preview TEXT NOT NULL CHECK(length(preview)<=200),
 status TEXT NOT NULL CHECK(status IN ('working','replied','failed','stopped')),
 created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
 updated_at INTEGER NOT NULL CHECK(updated_at BETWEEN 0 AND 9007199254740991)
) WITHOUT ROWID;
CREATE TABLE plugin_approvals(
 plugin_id TEXT NOT NULL,
 version TEXT NOT NULL,
 digest TEXT NOT NULL CHECK(length(digest)=64),
 PRIMARY KEY(plugin_id,version),
 FOREIGN KEY(plugin_id,version) REFERENCES plugin_versions(plugin_id,version) ON DELETE CASCADE
) WITHOUT ROWID;
INSERT INTO "plugin_approvals" VALUES('discord','0.4.1','8e2b1bce01952660db7def45d9dcd4e0d21eefbe95064c5fb612c3ef2c599953');
INSERT INTO "plugin_approvals" VALUES('github','1.0.0','4668e988287568e4c4b04216bd0ec985b6d67ed9eb5ccb5b748f1d0cdc97200d');
INSERT INTO "plugin_approvals" VALUES('gmail','1.0.0','14b19aec94da4792bcecd51991785ae625db51c527aaa840f6bea93ac2626122');
INSERT INTO "plugin_approvals" VALUES('onepassword','1.2.0','fd009de7b31b24d2f47581f1165a9de3250f28b4c90b09f33f5f6e5a8c6e2798');
CREATE TABLE plugin_deliveries(
 id TEXT PRIMARY KEY NOT NULL,
 instance_id TEXT NOT NULL REFERENCES plugin_instances(id) ON DELETE CASCADE,
 revision TEXT NOT NULL CHECK(length(revision)>0),
 chat_id TEXT NOT NULL,
 thread TEXT NOT NULL,
 user_id TEXT NOT NULL CHECK(length(user_id) BETWEEN 1 AND 64),
 reply TEXT NOT NULL CHECK(json_valid(reply)),
 state TEXT NOT NULL CHECK(json_valid(state)),
 asked TEXT NOT NULL CHECK(json_valid(asked) AND json_type(asked)='array'),
 question_retry_at TEXT NOT NULL CHECK(json_valid(question_retry_at) AND json_type(question_retry_at)='object'),
 progress_at INTEGER NOT NULL CHECK(progress_at BETWEEN 0 AND 9007199254740991),
 attempts INTEGER NOT NULL CHECK(attempts BETWEEN 0 AND 5),
 created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
 next_attempt_at INTEGER NOT NULL CHECK(next_attempt_at BETWEEN 0 AND 9007199254740991)
) WITHOUT ROWID;
CREATE TABLE plugin_events(
 id TEXT PRIMARY KEY NOT NULL,
 instance_id TEXT NOT NULL REFERENCES plugin_instances(id) ON DELETE CASCADE,
 revision TEXT NOT NULL CHECK(length(revision)>0),
 name TEXT NOT NULL,
 payload TEXT NOT NULL CHECK(json_valid(payload)),
 author_id TEXT CHECK(author_id IS NULL OR length(author_id) BETWEEN 1 AND 64),
 attempts INTEGER NOT NULL CHECK(attempts BETWEEN 0 AND 5),
 created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
 next_attempt_at INTEGER NOT NULL CHECK(next_attempt_at BETWEEN 0 AND 9007199254740991)
) WITHOUT ROWID;
CREATE TABLE plugin_http_receipts(
 id TEXT PRIMARY KEY NOT NULL,
 instance_id TEXT NOT NULL REFERENCES plugin_instances(id) ON DELETE CASCADE,
 fingerprint TEXT NOT NULL CHECK(length(fingerprint)=64),
 response TEXT CHECK(response IS NULL OR json_valid(response)),
 created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991)
) WITHOUT ROWID;
CREATE TABLE plugin_instances(
 id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=36),
 plugin_id TEXT NOT NULL REFERENCES plugin_packages(id) CHECK(length(plugin_id) BETWEEN 1 AND 80),
 name TEXT NOT NULL CHECK(length(trim(name)) BETWEEN 1 AND 100),
 enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),
 revision TEXT NOT NULL CHECK(length(revision)>0),
 settings TEXT NOT NULL CHECK(json_valid(settings) AND json_type(settings)='object'),
 secret_keys TEXT NOT NULL CHECK(json_valid(secret_keys) AND json_type(secret_keys)='array'),
 owner_access_id TEXT,
 updated_at INTEGER NOT NULL CHECK(updated_at BETWEEN 0 AND 9007199254740991),
 state TEXT NOT NULL DEFAULT 'disconnected' CHECK(state IN ('disconnected','connecting','connected','needs_review','ready')),
 state_message TEXT,
 state_updated_at INTEGER
) WITHOUT ROWID;
CREATE TABLE plugin_packages(
 id TEXT PRIMARY KEY NOT NULL CHECK(length(id) BETWEEN 1 AND 80),
 active_version TEXT,
 revision TEXT NOT NULL CHECK(length(revision)>0),
 updated_at INTEGER NOT NULL CHECK(updated_at BETWEEN 0 AND 9007199254740991),
 FOREIGN KEY(id,active_version) REFERENCES plugin_versions(plugin_id,version) DEFERRABLE INITIALLY DEFERRED
) WITHOUT ROWID;
INSERT INTO "plugin_packages" VALUES('discord','0.4.1','94af72c2-f83e-4eab-9b1f-6c8eb021ffea',1793252225264);
INSERT INTO "plugin_packages" VALUES('github','1.0.0','8cc91cef-aa92-4cd0-80e8-cea04d71413f',1793252225286);
INSERT INTO "plugin_packages" VALUES('gmail','1.0.0','a82893b4-5847-4dd8-a34e-5991cae792b7',1793252225295);
INSERT INTO "plugin_packages" VALUES('onepassword','1.2.0','04c6c984-5dcf-4877-a4fb-6a94d5cbfc16',1793252225303);
CREATE TABLE plugin_requests(
 id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=36),
 instance_id TEXT NOT NULL REFERENCES plugin_instances(id) ON DELETE CASCADE,
 user_id TEXT NOT NULL CHECK(length(user_id) BETWEEN 1 AND 64),
 name TEXT NOT NULL CHECK(length(name)<=100),
 status TEXT NOT NULL CHECK(status IN ('pending','denied')),
 mentions INTEGER NOT NULL CHECK(mentions BETWEEN 1 AND 4294967295),
 created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
 updated_at INTEGER NOT NULL CHECK(updated_at BETWEEN 0 AND 9007199254740991),
 UNIQUE(instance_id,user_id)
) WITHOUT ROWID;
CREATE TABLE plugin_storage(
 instance_id TEXT NOT NULL REFERENCES plugin_instances(id) ON DELETE CASCADE,
 key TEXT NOT NULL CHECK(length(key) BETWEEN 1 AND 200),
 value TEXT NOT NULL CHECK(json_valid(value) AND length(CAST(value AS BLOB))<=65536),
 updated_at INTEGER NOT NULL CHECK(updated_at BETWEEN 0 AND 9007199254740991),
 PRIMARY KEY(instance_id,key)
) WITHOUT ROWID;
CREATE TABLE plugin_timers(
 id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=36),
 instance_id TEXT NOT NULL REFERENCES plugin_instances(id) ON DELETE CASCADE,
 revision TEXT NOT NULL CHECK(length(revision)>0),
 name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 64),
 payload TEXT NOT NULL CHECK(json_valid(payload) AND length(CAST(payload AS BLOB))<=4096),
 at INTEGER NOT NULL CHECK(at BETWEEN 0 AND 9007199254740991)
) WITHOUT ROWID;
CREATE TABLE plugin_versions(
 plugin_id TEXT NOT NULL REFERENCES plugin_packages(id) ON DELETE CASCADE,
 version TEXT NOT NULL CHECK(length(version) BETWEEN 1 AND 80),
 digest TEXT NOT NULL CHECK(length(digest)=64),
 PRIMARY KEY(plugin_id,version)
) WITHOUT ROWID;
INSERT INTO "plugin_versions" VALUES('discord','0.4.1','8e2b1bce01952660db7def45d9dcd4e0d21eefbe95064c5fb612c3ef2c599953');
INSERT INTO "plugin_versions" VALUES('github','1.0.0','4668e988287568e4c4b04216bd0ec985b6d67ed9eb5ccb5b748f1d0cdc97200d');
INSERT INTO "plugin_versions" VALUES('gmail','1.0.0','14b19aec94da4792bcecd51991785ae625db51c527aaa840f6bea93ac2626122');
INSERT INTO "plugin_versions" VALUES('onepassword','1.2.0','fd009de7b31b24d2f47581f1165a9de3250f28b4c90b09f33f5f6e5a8c6e2798');
CREATE TABLE projects(
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL CHECK(length(trim(name)) BETWEEN 1 AND 100),
    path TEXT NOT NULL CHECK(length(path) BETWEEN 1 AND 2000),
    source_mode TEXT NOT NULL CHECK(source_mode IN ('remote','local')),
    base_branch TEXT NOT NULL CHECK(length(base_branch)>0),
    description TEXT NOT NULL CHECK(length(description)<=500),
    origin TEXT NOT NULL,
    source_plugin_id TEXT,
    source_name TEXT,
    source_instance_id TEXT,
    source_item_id TEXT,
    revision TEXT NOT NULL CHECK(length(revision)>0),
    created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
    updated_at INTEGER NOT NULL,
    CHECK((source_plugin_id IS NULL AND source_name IS NULL AND source_instance_id IS NULL AND source_item_id IS NULL)
       OR (source_plugin_id IS NOT NULL AND source_name IS NOT NULL AND source_instance_id IS NOT NULL AND source_item_id IS NOT NULL))
);
CREATE TABLE records(id TEXT PRIMARY KEY,kind TEXT NOT NULL CHECK(kind NOT IN ('nodes','projects','agents','tasks','mcps','plugin-connections','plugin-packages','plugin-requests','plugin-activity','plugin-timers','plugin-events','plugin-deliveries','plugin-storage','plugin-http-receipts')),data TEXT NOT NULL,updated_at INTEGER NOT NULL);
INSERT INTO "records" VALUES('00000000-0000-4000-8000-000000000001','agentAccounts','{"checkedAt":1700000000123,"createdAt":1700000000000,"email":"account0@example.test","enabled":true,"error":"","exhausted":{"at":1700000000120,"model":"gpt-spark","usage":{"allowed":true,"attemptedAt":1700000000124,"checkedAt":1700000000123,"error":"Retained poll failure","resets":{"available":2,"credits":[{"expiresAt":1900000000,"id":"synthetic-credit"}]},"useBalance":true,"windows":[{"durationMins":10080,"id":"weekly","label":"Weekly","models":[],"reached":false,"resetsAt":1900000000,"usedPercent":87.123456789},{"durationMins":300,"id":"model","label":"Model","models":["gpt-6.1-sol","gpt-spark"],"reached":true,"resetsAt":1900000001,"usedPercent":100}]}},"id":"00000000-0000-4000-8000-000000000001","identity":"synthetic-identity-0","lastUsedAt":1700000000099,"maxConcurrentRuns":7,"name":"Retained Codex","plan":"Retained plan","priority":2,"provider":"codex","resetError":"Retained reset status","state":"ready","usage":{"allowed":true,"attemptedAt":1700000000124,"checkedAt":1700000000123,"error":"Retained poll failure","resets":{"available":2,"credits":[{"expiresAt":1900000000,"id":"synthetic-credit"}]},"useBalance":true,"windows":[{"durationMins":10080,"id":"weekly","label":"Weekly","models":[],"reached":false,"resetsAt":1900000000,"usedPercent":87.123456789},{"durationMins":300,"id":"model","label":"Model","models":["gpt-6.1-sol","gpt-spark"],"reached":true,"resetsAt":1900000001,"usedPercent":100}]}}',1793252225652);
INSERT INTO "records" VALUES('00000000-0000-4000-8000-000000000002','agentAccounts','{"checkedAt":1700000000123,"createdAt":1700000000001,"email":"account1@example.test","enabled":false,"error":"","exhausted":null,"id":"00000000-0000-4000-8000-000000000002","identity":"synthetic-identity-1","lastUsedAt":1700000000099,"maxConcurrentRuns":7,"name":"Retained Claude","plan":"Retained plan","priority":1,"provider":"claude","resetError":"Retained reset status","state":"ready","usage":null}',1793252225757);
INSERT INTO "records" VALUES('00000000-0000-4000-8000-000000000003','agentAccounts','{"checkedAt":1700000000123,"createdAt":1700000000002,"email":"account2@example.test","enabled":true,"error":"","exhausted":null,"id":"00000000-0000-4000-8000-000000000003","identity":"synthetic-identity-2","lastUsedAt":1700000000099,"maxConcurrentRuns":7,"modelProvider":"opencode-go","modelProviderName":"OpenCode Go","name":"Retained Opencode","plan":"Retained plan","priority":0,"provider":"opencode","resetError":"Retained reset status","state":"ready","usage":{"allowed":true,"attemptedAt":1700000000124,"checkedAt":1700000000123,"error":"Retained poll failure","resets":{"available":2,"credits":[{"expiresAt":1900000000,"id":"synthetic-credit"}]},"useBalance":true,"windows":[{"durationMins":10080,"id":"weekly","label":"Weekly","models":[],"reached":false,"resetsAt":1900000000,"usedPercent":87.123456789},{"durationMins":300,"id":"model","label":"Model","models":["gpt-6.1-sol","gpt-spark"],"reached":true,"resetsAt":1900000001,"usedPercent":100}]}}',1793252225866);
INSERT INTO "records" VALUES('retained-chat','chats','{"accountId":"00000000-0000-4000-8000-000000000001","id":"retained-chat","slug":"little-moon-480bd6dd"}',1793252225976);
CREATE TABLE remote_deletions(id TEXT PRIMARY KEY,location TEXT NOT NULL,key TEXT NOT NULL,prefix INTEGER NOT NULL DEFAULT 0,retry_at INTEGER NOT NULL DEFAULT 0);
CREATE TABLE run_checkpoints(run_id TEXT PRIMARY KEY NOT NULL REFERENCES runs(id) ON DELETE CASCADE,state TEXT NOT NULL CHECK(json_valid(state) AND json_type(state)='object'));
INSERT INTO "run_checkpoints" VALUES('00000000-0000-4000-8000-000000000010','{}');
CREATE TABLE runs(
  id TEXT PRIMARY KEY NOT NULL CHECK(length(id)>0),
  task_id TEXT NOT NULL CHECK(length(task_id)>0),
  project_id TEXT,
  status TEXT NOT NULL CHECK(status IN ('queued','running','waiting','succeeded','failed','cancelled','interrupted')),
  trigger TEXT NOT NULL DEFAULT '',
  created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
  started_at INTEGER CHECK(started_at BETWEEN 0 AND 9007199254740991),
  finished_at INTEGER CHECK(finished_at BETWEEN 0 AND 9007199254740991),
  summary TEXT NOT NULL DEFAULT '',
  error TEXT,
  session_id TEXT,
  workspace TEXT,
  workspace_cleaned_at INTEGER CHECK(workspace_cleaned_at BETWEEN 0 AND 9007199254740991),
  recovery_pending INTEGER NOT NULL CHECK(recovery_pending IN (0,1)),
  resume_available INTEGER NOT NULL CHECK(resume_available IN (0,1)),
  resume_count INTEGER NOT NULL CHECK(resume_count BETWEEN 0 AND 4294967295),
  cancel_requested_at INTEGER CHECK(cancel_requested_at BETWEEN 0 AND 9007199254740991),
  account_id TEXT,
  account_name TEXT,
  account_wait_reason TEXT,
  account_required TEXT CHECK(account_required IN ('codex','claude','opencode')),
  capacity_wait_until INTEGER CHECK(capacity_wait_until BETWEEN 0 AND 9007199254740991),
  retry_attempt INTEGER CHECK(retry_attempt BETWEEN 1 AND 5),
  retry_limit INTEGER CHECK(retry_limit=5),
  retry_next_at INTEGER CHECK(retry_next_at BETWEEN 0 AND 9007199254740991),
  retry_started_at INTEGER CHECK(retry_started_at BETWEEN 0 AND 9007199254740991),
  retry_deadline_at INTEGER CHECK(retry_deadline_at BETWEEN 0 AND 9007199254740991),
  retry_cause TEXT CHECK(retry_cause IS NULL OR json_valid(retry_cause)),
  requested_by TEXT CHECK(requested_by IS NULL OR json_valid(requested_by)),
  -- Frozen display fields are derived by SQLite once per snapshot write. Lists
  -- need neither the snapshot's overflow pages nor repeated JSON extraction.
  task_name TEXT GENERATED ALWAYS AS (coalesce(json_extract(snapshot,'$.task.name'),'')) STORED,
  agent_name TEXT GENERATED ALWAYS AS (coalesce(json_extract(snapshot,'$.agent.name'),'')) STORED,
  provider TEXT GENERATED ALWAYS AS (json_extract(snapshot,'$.agent.provider')) STORED,
  snapshot TEXT CHECK(snapshot IS NULL OR json_valid(snapshot)),
  boundary TEXT NOT NULL CHECK(json_valid(boundary) AND json_type(boundary)='object' AND json_type(boundary,'$.snapshot') IS NULL),
  dedupe TEXT UNIQUE,
  data TEXT GENERATED ALWAYS AS (json_set(boundary,'$.snapshot',json(snapshot),'$.summary',summary,

  '$.id',id,
  '$.taskId',task_id,
  '$.projectId',project_id,
  '$.status',status,
  '$.trigger',trigger,
  '$.createdAt',created_at,
  '$.startedAt',started_at,
  '$.finishedAt',finished_at,
  '$.error',error,
  '$.sessionId',session_id,
  '$.workspace',workspace,
  '$.workspaceCleanedAt',workspace_cleaned_at,
  '$.recoveryPending',json(CASE WHEN recovery_pending THEN 'true' ELSE 'false' END),
  '$.resumeAvailable',json(CASE WHEN resume_available THEN 'true' ELSE 'false' END),
  '$.resumeCount',resume_count,
  '$.cancelRequestedAt',cancel_requested_at,
  '$.accountId',account_id,
  '$.accountName',account_name,
  '$.accountWaitReason',account_wait_reason,
  '$.accountRequired',account_required,
  '$.capacityWaitUntil',capacity_wait_until,
  '$.requestedBy',json(requested_by),
  '$.retry',json(CASE WHEN retry_attempt IS NULL THEN NULL ELSE json_object('attempt',retry_attempt,'limit',retry_limit,'nextAttemptAt',retry_next_at,'startedAt',retry_started_at,'deadlineAt',retry_deadline_at,'cause',json(retry_cause)) END)
)) VIRTUAL,
  summary_data TEXT GENERATED ALWAYS AS (json_set(boundary,

  '$.id',id,
  '$.taskId',task_id,
  '$.projectId',project_id,
  '$.status',status,
  '$.trigger',trigger,
  '$.createdAt',created_at,
  '$.startedAt',started_at,
  '$.finishedAt',finished_at,
  '$.error',error,
  '$.sessionId',session_id,
  '$.workspace',workspace,
  '$.workspaceCleanedAt',workspace_cleaned_at,
  '$.recoveryPending',json(CASE WHEN recovery_pending THEN 'true' ELSE 'false' END),
  '$.resumeAvailable',json(CASE WHEN resume_available THEN 'true' ELSE 'false' END),
  '$.resumeCount',resume_count,
  '$.cancelRequestedAt',cancel_requested_at,
  '$.accountId',account_id,
  '$.accountName',account_name,
  '$.accountWaitReason',account_wait_reason,
  '$.accountRequired',account_required,
  '$.capacityWaitUntil',capacity_wait_until,
  '$.requestedBy',json(requested_by),
  '$.retry',json(CASE WHEN retry_attempt IS NULL THEN NULL ELSE json_object('attempt',retry_attempt,'limit',retry_limit,'nextAttemptAt',retry_next_at,'startedAt',retry_started_at,'deadlineAt',retry_deadline_at,'cause',json(retry_cause)) END)
,'$.taskName',task_name,'$.agentName',agent_name,'$.provider',provider)) VIRTUAL,
  CHECK((retry_attempt IS NULL AND retry_limit IS NULL AND retry_cause IS NULL AND retry_next_at IS NULL AND retry_started_at IS NULL AND retry_deadline_at IS NULL) OR (retry_attempt IS NOT NULL AND retry_limit IS NOT NULL AND retry_cause IS NOT NULL)),
  CHECK(retry_deadline_at IS NULL OR retry_started_at IS NULL OR retry_deadline_at>=retry_started_at)
);
INSERT INTO "runs" VALUES('00000000-0000-4000-8000-000000000010','00000000-0000-4000-8000-000000000011',NULL,'interrupted','',1700000000001,NULL,NULL,'',NULL,'synthetic-session',NULL,NULL,1,1,3,NULL,'00000000-0000-4000-8000-000000000001','Retained Codex',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'{}',NULL);
CREATE TABLE shared_objects(id TEXT PRIMARY KEY,destination TEXT NOT NULL,hash TEXT NOT NULL,size INTEGER NOT NULL,location TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN ('pending','ready','invalid','deleting')),unused_at INTEGER,retry_at INTEGER NOT NULL DEFAULT 0);
CREATE TABLE shared_publications(id TEXT PRIMARY KEY,run_id TEXT NOT NULL);
CREATE TABLE shared_references(publication TEXT NOT NULL REFERENCES shared_publications(id) ON DELETE CASCADE,object TEXT NOT NULL REFERENCES shared_objects(id),PRIMARY KEY(publication,object));
CREATE INDEX records_kind ON records(kind,updated_at DESC);
CREATE INDEX records_node_backups_run ON records(json_extract(data,'$.runId')) WHERE kind='node-backups';
CREATE INDEX events_run ON events(run_id,id);
CREATE INDEX chat_messages_chat ON chat_messages(chat_id,created_at);
CREATE INDEX events_turns ON events(run_id,id) WHERE type='turn.started';
CREATE INDEX events_user_input ON events(run_id,id) WHERE type='chat.user';
CREATE INDEX events_messages ON events(run_id,json_extract(payload,'$.item.id'),id) WHERE json_extract(payload,'$.item.type')='agent_message';
CREATE INDEX runs_status ON runs(status,created_at);
CREATE INDEX runs_agent ON runs(json_extract(snapshot,'$.agent.id'));
CREATE INDEX runs_task ON runs(task_id,created_at DESC);
CREATE INDEX runs_created ON runs(created_at DESC,id DESC);
CREATE UNIQUE INDEX runs_active_task ON runs(task_id) WHERE status IN ('queued','running','waiting');
CREATE INDEX shared_publications_run ON shared_publications(run_id);
CREATE UNIQUE INDEX shared_objects_reuse ON shared_objects(destination,hash) WHERE state='ready';
CREATE INDEX shared_objects_garbage ON shared_objects(retry_at,unused_at) WHERE unused_at IS NOT NULL;
CREATE INDEX shared_references_object ON shared_references(object);
CREATE INDEX remote_deletions_due ON remote_deletions(retry_at);
CREATE INDEX projects_updated ON projects(updated_at DESC,id);
CREATE INDEX projects_source ON projects(source_plugin_id,source_item_id);
CREATE INDEX nodes_inventory ON nodes(updated_at DESC,id);
CREATE INDEX agents_updated ON agents(updated_at DESC,id);
CREATE INDEX mcp_servers_updated ON mcp_servers(updated_at DESC,id);
CREATE INDEX mcp_run_grants_run ON mcp_run_grants(run_id);
CREATE INDEX mcp_pending_server_session ON mcp_pending_oauth(server_id,session);
CREATE INDEX plugin_instances_updated ON plugin_instances(updated_at DESC,id);
CREATE INDEX plugin_instances_package ON plugin_instances(plugin_id,id);
CREATE INDEX external_delegations_agent ON external_delegations(agent_id,instance_id);
CREATE INDEX external_delegations_project ON external_delegations(project_id,instance_id);
CREATE INDEX plugin_requests_updated ON plugin_requests(updated_at DESC,id);
CREATE INDEX plugin_activity_recent ON plugin_activity(instance_id,created_at DESC,id);
CREATE INDEX plugin_activity_delivery ON plugin_activity(delivery_id,id);
CREATE INDEX plugin_timers_due ON plugin_timers(instance_id,at,id);
CREATE INDEX plugin_events_instance ON plugin_events(instance_id,created_at,id,next_attempt_at);
CREATE INDEX plugin_deliveries_instance ON plugin_deliveries(instance_id,created_at,id,next_attempt_at);
CREATE INDEX plugin_http_receipts_instance ON plugin_http_receipts(instance_id,id);
CREATE INDEX agent_projects_resource ON agent_projects(project_id,agent_id);
CREATE INDEX agent_nodes_resource ON agent_nodes(node_id,agent_id);
CREATE INDEX agent_mcps_resource ON agent_mcps(mcp_id,agent_id);
CREATE INDEX instance_grants_resource ON instance_grants(instance_id,agent_id);
CREATE INDEX agent_exclusive_nodes_resource ON agent_exclusive_nodes(node_id,agent_id);
CREATE TRIGGER missions_author_immutable BEFORE UPDATE ON missions
 WHEN OLD.author_id IS NOT NEW.author_id OR OLD.author_access_id IS NOT NEW.author_access_id
   OR (OLD.author_removed=1 AND NEW.author_removed=0)
 BEGIN SELECT RAISE(ABORT,'Mission authorship is immutable'); END;
CREATE INDEX missions_updated ON missions(updated_at DESC,id);
CREATE INDEX missions_due ON missions(next_run,id) WHERE enabled=1 AND archived=0 AND cron IS NOT NULL;
CREATE INDEX missions_agent ON missions(agent_id);
CREATE INDEX missions_project ON missions(project_id) WHERE project_id IS NOT NULL;
CREATE UNIQUE INDEX records_chat_slug ON records(json_extract(data,'$.slug')) WHERE kind='chats';
DELETE FROM "sqlite_sequence";
COMMIT;
PRAGMA user_version=12;
PRAGMA foreign_keys=ON;
