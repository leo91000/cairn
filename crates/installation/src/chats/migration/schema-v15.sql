
CREATE TABLE conversation_retention(singleton INTEGER PRIMARY KEY CHECK(singleton=1),inactivity_days INTEGER NOT NULL CHECK(inactivity_days BETWEEN 0 AND 36500));
CREATE TABLE conversations(
    id TEXT PRIMARY KEY NOT NULL,
    slug TEXT NOT NULL UNIQUE CHECK(length(slug)>0),
    title TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    live_agent_id TEXT REFERENCES agents(id) ON DELETE SET NULL CHECK(live_agent_id=agent_id),
    project_id TEXT,
    live_project_id TEXT REFERENCES projects(id) ON DELETE SET NULL CHECK(live_project_id=project_id),
    run_id TEXT REFERENCES runs(id) ON DELETE SET NULL,
    paused INTEGER NOT NULL CHECK(paused IN (0,1)),
    lifecycle TEXT NOT NULL CHECK(lifecycle IN ('active','trash','purging')),
    created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
    updated_at INTEGER NOT NULL CHECK(updated_at BETWEEN 0 AND 9007199254740991),
    last_activity_at INTEGER CHECK(last_activity_at BETWEEN 0 AND 9007199254740991),
    revision TEXT NOT NULL CHECK(length(revision)>0),
    trashed_at INTEGER CHECK(trashed_at BETWEEN 0 AND 9007199254740991),
    purge_at INTEGER CHECK(purge_at BETWEEN 0 AND 9007199254740991),
    restored_at INTEGER CHECK(restored_at BETWEEN 0 AND 9007199254740991),
    retry_after INTEGER CHECK(retry_after BETWEEN 0 AND 9007199254740991),
    lifecycle_error TEXT,
    previous_lifecycle TEXT CHECK(previous_lifecycle IN ('active','trash','purging')),
    session_restart_requested INTEGER NOT NULL CHECK(session_restart_requested IN (0,1)),
    cancelled_by_deletion INTEGER NOT NULL CHECK(cancelled_by_deletion IN (0,1)),
    agent_name TEXT,
    project_name TEXT,
    requested_by TEXT CHECK(requested_by IS NULL OR json_valid(requested_by)),
    wakeup TEXT CHECK(wakeup IS NULL OR json_valid(wakeup)),
    native_principals TEXT CHECK(native_principals IS NULL OR json_valid(native_principals)),
    native_authority_nodes TEXT CHECK(native_authority_nodes IS NULL OR json_valid(native_authority_nodes)),
    native_parent_run_id TEXT,
    native_parent_access TEXT,
    native_provenance_unknown INTEGER CHECK(native_provenance_unknown IN (0,1)),
    native_plugin_grants TEXT CHECK(native_plugin_grants IS NULL OR json_valid(native_plugin_grants)) ,
    environment_state TEXT CHECK(environment_state IN ('cleaning','expired')),
    environment_expired_at INTEGER CHECK(environment_expired_at BETWEEN 0 AND 9007199254740991),
    environment_error TEXT,
    environment_retry_after INTEGER CHECK(environment_retry_after BETWEEN 0 AND 9007199254740991),
    account_id TEXT,
    persisted_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX conversations_run ON conversations(run_id) WHERE run_id IS NOT NULL;
CREATE INDEX conversations_live_agent ON conversations(live_agent_id) WHERE live_agent_id IS NOT NULL;
CREATE INDEX conversations_live_project ON conversations(live_project_id) WHERE live_project_id IS NOT NULL;
CREATE INDEX conversations_updated ON conversations(persisted_at DESC,id);
CREATE TABLE chat_messages(
    id TEXT PRIMARY KEY NOT NULL,
    chat_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    revision TEXT NOT NULL CHECK(length(revision)>0),
    text TEXT NOT NULL,
    provider TEXT NOT NULL CHECK(provider IN ('','codex','claude','opencode')),
    model TEXT NOT NULL,
    reasoning TEXT NOT NULL,
    speed TEXT NOT NULL,
    mode TEXT NOT NULL CHECK(mode IN ('queue','steer')),
    status TEXT NOT NULL CHECK(status IN ('queued','sending','delivered','cancelled')),
    created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
    question_id TEXT,
    answers TEXT CHECK(answers IS NULL OR json_valid(answers))
);
CREATE INDEX chat_messages_chat ON chat_messages(chat_id,created_at);
CREATE INDEX chat_messages_pending ON chat_messages(chat_id,status,created_at);
CREATE TABLE message_attachments(
    message_id TEXT NOT NULL REFERENCES chat_messages(id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 7),
    attachment_id TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    name TEXT NOT NULL,
    size INTEGER NOT NULL CHECK(size BETWEEN 0 AND 41943040),
    media_type TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('image','file')),
    width INTEGER CHECK(width>0),
    height INTEGER CHECK(height>0),
    digest TEXT NOT NULL,
    created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
    PRIMARY KEY(message_id,position),
    UNIQUE(message_id,attachment_id)
) WITHOUT ROWID;
