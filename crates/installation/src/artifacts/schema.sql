CREATE TABLE artifacts(
 run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
 key TEXT NOT NULL CHECK(length(key) BETWEEN 1 AND 160),
 PRIMARY KEY(run_id,key)
) WITHOUT ROWID;
CREATE TABLE artifact_versions(
 id TEXT PRIMARY KEY NOT NULL,
 run_id TEXT NOT NULL,
 key TEXT NOT NULL,
 version INTEGER NOT NULL CHECK(version BETWEEN 1 AND 9007199254740991),
 message_id TEXT,
 title TEXT NOT NULL CHECK(length(title) BETWEEN 1 AND 160),
 name TEXT NOT NULL,
 group_name TEXT NOT NULL CHECK(length(group_name)<=160),
 kind TEXT NOT NULL CHECK(kind IN ('image','video','audio','pdf','markdown','code','file')),
 media_type TEXT NOT NULL,
 size INTEGER NOT NULL CHECK(size BETWEEN 0 AND 536870912),
 digest TEXT,
 created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
 url TEXT NOT NULL,
 preview_status TEXT NOT NULL CHECK(preview_status IN ('pending','ready','unavailable','none')),
 width INTEGER CHECK(width BETWEEN 0 AND 4294967295),
 height INTEGER CHECK(height BETWEEN 0 AND 4294967295),
 duration REAL CHECK(duration>=0 AND duration<=1.7976931348623157e308),
 excerpt TEXT,
 FOREIGN KEY(run_id,key) REFERENCES artifacts(run_id,key) ON DELETE CASCADE,
 UNIQUE(run_id,key,version)
);
CREATE INDEX artifact_versions_run ON artifact_versions(run_id,created_at,id);
CREATE TABLE artifact_visibility(
 version_id TEXT PRIMARY KEY NOT NULL REFERENCES artifact_versions(id) ON DELETE CASCADE,
 visibility TEXT NOT NULL CHECK(visibility IN ('private','public')),
 public_token TEXT UNIQUE,
 public_url TEXT,
 CHECK((visibility='public' AND public_token IS NOT NULL AND public_url IS NOT NULL)
    OR (visibility='private' AND public_token IS NULL AND public_url IS NULL))
) WITHOUT ROWID;
