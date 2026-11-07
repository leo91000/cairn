-- Offline migration only. Stop the installation and back up manager.db first.
-- Run with sqlite3 -bail manager.db < docs/migrations/0011-mixed-node-agents.sql
PRAGMA foreign_keys = ON;
BEGIN IMMEDIATE;
CREATE TEMP TABLE migration_version_guard(version INTEGER NOT NULL CHECK(version=10));
INSERT INTO migration_version_guard SELECT user_version FROM pragma_user_version;

CREATE TABLE agent_permissions_v11(
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
INSERT INTO agent_permissions_v11 SELECT * FROM agent_permissions;
DROP TABLE agent_permissions;
ALTER TABLE agent_permissions_v11 RENAME TO agent_permissions;

CREATE TABLE agent_node_priorities(
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 node_id TEXT NOT NULL,
 position INTEGER NOT NULL CHECK(position>=0),
 PRIMARY KEY(agent_id,node_id), UNIQUE(agent_id,position)
) WITHOUT ROWID;

DROP TABLE migration_version_guard;
PRAGMA user_version = 11;
COMMIT;
