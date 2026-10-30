CREATE TABLE skill_sources(
 scope TEXT NOT NULL,
 name TEXT NOT NULL,
 project_id TEXT REFERENCES projects(id) ON DELETE SET NULL,
 plugin_id TEXT NOT NULL,
 source TEXT NOT NULL,
 recorded_instance TEXT NOT NULL,
 location TEXT NOT NULL CHECK(json_valid(location) AND json_type(location)='object'),
 folder TEXT NOT NULL,
 version TEXT NOT NULL,
 label TEXT NOT NULL,
 url TEXT,
 imported_at INTEGER NOT NULL CHECK(imported_at BETWEEN 0 AND 9007199254740991),
 revision TEXT NOT NULL,
 PRIMARY KEY(scope,name),
 CHECK((scope='global' AND project_id IS NULL) OR (scope!='global' AND (project_id IS NULL OR scope=project_id)))
) WITHOUT ROWID;
-- Keep provenance when a plugin/instance is removed; only the live reference is nullable.
CREATE TABLE skill_source_refs(
 scope TEXT NOT NULL,
 name TEXT NOT NULL,
 instance_id TEXT REFERENCES plugin_instances(id) ON DELETE SET NULL,
 PRIMARY KEY(scope,name),
 FOREIGN KEY(scope,name) REFERENCES skill_sources(scope,name) ON DELETE CASCADE
) WITHOUT ROWID;
