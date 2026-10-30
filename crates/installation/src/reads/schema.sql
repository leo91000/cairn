CREATE TABLE account_read_starts(
    account_id TEXT PRIMARY KEY NOT NULL CHECK(length(account_id) BETWEEN 1 AND 200),
    started_at INTEGER NOT NULL CHECK(started_at BETWEEN 0 AND 9007199254740991)
) WITHOUT ROWID;
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
