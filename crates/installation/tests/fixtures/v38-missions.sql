-- Exact v38 DDL from f04ce3f9 for tables replaced or removed by the mission sunset.

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
CREATE TRIGGER missions_author_immutable BEFORE UPDATE ON missions
 WHEN OLD.author_id IS NOT NEW.author_id OR OLD.author_access_id IS NOT NEW.author_access_id
   OR (OLD.author_removed=1 AND NEW.author_removed=0)
 BEGIN SELECT RAISE(ABORT,'Mission authorship is immutable'); END;
CREATE INDEX missions_updated ON missions(updated_at DESC,id);
CREATE INDEX missions_due ON missions(next_run,id) WHERE enabled=1 AND archived=0 AND cron IS NOT NULL;
CREATE INDEX missions_agent ON missions(agent_id);
CREATE INDEX missions_project ON missions(project_id) WHERE project_id IS NOT NULL;
CREATE TABLE mission_tags(
 mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
 position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 9),
 tag TEXT NOT NULL CHECK(length(tag)<=30),
 PRIMARY KEY(mission_id,position)
) WITHOUT ROWID;
CREATE TABLE mission_skills(
 mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
 position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 99),
 skill TEXT NOT NULL CHECK(length(skill)<=160),
 PRIMARY KEY(mission_id,position)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS hook_rule_task_depths(
 task_id TEXT PRIMARY KEY NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
 depth INTEGER NOT NULL CHECK(depth BETWEEN 0 AND 8)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS hook_rule_versions(
 rule_id TEXT NOT NULL REFERENCES hook_rules(id) ON DELETE CASCADE,
 version INTEGER NOT NULL CHECK(version>=1),
 definition TEXT NOT NULL CHECK(json_valid(definition) AND json_type(definition)='object' AND length(CAST(definition AS BLOB))<=524288),
 saved_by TEXT NOT NULL CHECK(length(saved_by) BETWEEN 1 AND 100),
 source TEXT NOT NULL CHECK(source IN ('app','publicMcp','installationAgent','duplicate','template')),
 conversation_id TEXT CHECK(conversation_id IS NULL OR length(conversation_id) BETWEEN 1 AND 100),
 native_grant TEXT NOT NULL CHECK(json_valid(native_grant) AND json_type(native_grant)='object'),
 saved_at INTEGER NOT NULL CHECK(saved_at BETWEEN 0 AND 9007199254740991),
 PRIMARY KEY(rule_id,version)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS hook_rule_uses(
 rule_id TEXT NOT NULL REFERENCES hook_rules(id) ON DELETE CASCADE,
 kind TEXT NOT NULL CHECK(kind IN ('executionAgent','startedAgent','hook','tool','cairnHook')),
 agent_id TEXT NOT NULL,
 instance_id TEXT NOT NULL,
 name TEXT NOT NULL CHECK(length(name)<=200),
 PRIMARY KEY(rule_id,kind,agent_id,instance_id,name)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS hook_rule_uses_agent ON hook_rule_uses(agent_id,kind);
CREATE INDEX IF NOT EXISTS hook_rule_uses_instance ON hook_rule_uses(instance_id,kind);

CREATE TABLE account_reads(
    account_id TEXT NOT NULL CHECK(length(account_id) BETWEEN 1 AND 200),
    kind TEXT NOT NULL CHECK(kind IN ('chat','mission')),
    item_id TEXT NOT NULL CHECK(length(item_id) BETWEEN 32 AND 45),
    finished_at INTEGER NOT NULL CHECK(finished_at BETWEEN 0 AND 9007199254740991),
    PRIMARY KEY(account_id,kind,item_id)
) WITHOUT ROWID;
CREATE TABLE account_dismissals(
    account_id TEXT NOT NULL CHECK(length(account_id) BETWEEN 1 AND 200),
    task_id TEXT NOT NULL CHECK(length(task_id) BETWEEN 32 AND 45),
    finished_at INTEGER NOT NULL CHECK(finished_at BETWEEN 1 AND 9007199254740991),
    snoozed_until INTEGER CHECK(snoozed_until BETWEEN finished_at + 1 AND 9007199254740991),
    PRIMARY KEY(account_id,task_id)
) WITHOUT ROWID;

CREATE TABLE durable_wakeups(
    key INTEGER PRIMARY KEY,
    chat_id TEXT REFERENCES conversations(id) ON DELETE CASCADE,
    mission_run_id TEXT REFERENCES runs(id) ON DELETE CASCADE,
    id TEXT NOT NULL CHECK(length(id)>0),
    message_id TEXT,
    reason TEXT NOT NULL,
    seconds INTEGER NOT NULL CHECK(seconds BETWEEN 1 AND 86400),
    created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
    wake_at INTEGER NOT NULL CHECK(wake_at BETWEEN 0 AND 9007199254740991),
    group_mode TEXT CHECK(group_mode IN ('all','any')),
    time_kind TEXT CHECK(time_kind IN ('delay','at')),
    time_seconds INTEGER CHECK(time_seconds BETWEEN 1 AND 86400),
    time_date TEXT,
    observation TEXT CHECK(observation IS NULL OR json_valid(observation)),
    display TEXT CHECK(display IS NULL OR json_valid(display)),
    next_check_at INTEGER CHECK(next_check_at BETWEEN 0 AND 9007199254740991),
    last_error TEXT,
    CHECK((chat_id IS NOT NULL) != (mission_run_id IS NOT NULL)),
    CHECK((time_kind IS NULL AND time_seconds IS NULL AND time_date IS NULL) OR (time_kind='delay' AND time_seconds IS NOT NULL AND time_date IS NULL) OR (time_kind='at' AND time_date IS NOT NULL AND time_seconds IS NULL))
);
CREATE UNIQUE INDEX durable_wakeups_chat ON durable_wakeups(chat_id) WHERE chat_id IS NOT NULL;
CREATE UNIQUE INDEX durable_wakeups_mission ON durable_wakeups(mission_run_id) WHERE mission_run_id IS NOT NULL;
CREATE TABLE wait_conditions(
    wakeup_key INTEGER NOT NULL REFERENCES durable_wakeups(key) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK(position>=0),
    kind TEXT NOT NULL CHECK(kind IN ('run_finished','task_finished','job_finished','delay','at','plugin')),
    target_run_id TEXT,
    target_chat_id TEXT,
    plugin_id TEXT,
    condition_type TEXT,
    project_id TEXT,
    plugin_input TEXT CHECK(plugin_input IS NULL OR (json_valid(plugin_input) AND json_type(plugin_input)='object')),
    observation TEXT CHECK(observation IS NULL OR json_valid(observation)),
    display TEXT CHECK(display IS NULL OR json_valid(display)),
    next_check_at INTEGER CHECK(next_check_at BETWEEN 0 AND 9007199254740991),
    last_error TEXT,
    CHECK((kind='run_finished' AND target_run_id IS NOT NULL AND target_chat_id IS NULL AND plugin_id IS NULL AND condition_type IS NULL AND project_id IS NULL AND plugin_input IS NULL)
       OR (kind='task_finished' AND target_chat_id IS NOT NULL AND target_run_id IS NULL AND plugin_id IS NULL AND condition_type IS NULL AND project_id IS NULL AND plugin_input IS NULL)
       OR (kind IN ('job_finished','delay','at') AND target_run_id IS NULL AND target_chat_id IS NULL AND plugin_id IS NULL AND condition_type IS NOT NULL AND project_id IS NULL AND plugin_input IS NULL)
       OR (kind='plugin' AND target_run_id IS NULL AND target_chat_id IS NULL AND plugin_id IS NOT NULL AND condition_type IS NOT NULL AND plugin_input IS NOT NULL)),
    PRIMARY KEY(wakeup_key,position)
) WITHOUT ROWID;
