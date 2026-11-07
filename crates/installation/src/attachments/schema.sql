CREATE TABLE attachment_quotas(
    chat_id TEXT PRIMARY KEY NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    committed_bytes INTEGER NOT NULL DEFAULT 0 CHECK(committed_bytes >= 0),
    reserved_bytes INTEGER NOT NULL DEFAULT 0 CHECK(reserved_bytes >= 0),
    CHECK(committed_bytes + reserved_bytes <= 209715200)
) WITHOUT ROWID;
CREATE TABLE attachments(
    id TEXT NOT NULL CHECK(length(id) BETWEEN 32 AND 45),
    chat_id TEXT NOT NULL REFERENCES attachment_quotas(chat_id) ON DELETE CASCADE,
    name TEXT NOT NULL CHECK(length(name)>0),
    size INTEGER NOT NULL CHECK(size BETWEEN 0 AND 10485760),
    media_type TEXT NOT NULL CHECK(length(media_type)>0),
    kind TEXT NOT NULL CHECK(kind IN ('image','file')),
    digest TEXT NOT NULL CHECK(length(digest)=64 AND digest NOT GLOB '*[^0-9a-f]*'),
    created_at INTEGER NOT NULL CHECK(created_at BETWEEN 0 AND 9007199254740991),
    width INTEGER CHECK(width BETWEEN 1 AND 4294967295),
    height INTEGER CHECK(height BETWEEN 1 AND 4294967295),
    storage_name TEXT NOT NULL CHECK(length(storage_name) BETWEEN 32 AND 82 AND instr(storage_name,'/')=0 AND instr(storage_name,char(92))=0),
    PRIMARY KEY(chat_id,id)
) WITHOUT ROWID;
CREATE INDEX attachments_order ON attachments(chat_id,created_at,id);
CREATE TABLE attachment_reservations(
    chat_id TEXT NOT NULL REFERENCES attachment_quotas(chat_id) ON DELETE CASCADE,
    id TEXT NOT NULL CHECK(length(id) BETWEEN 32 AND 45),
    token TEXT NOT NULL UNIQUE CHECK(length(token)=36),
    digest TEXT NOT NULL CHECK(length(digest)=64 AND digest NOT GLOB '*[^0-9a-f]*'),
    size INTEGER NOT NULL CHECK(size BETWEEN 0 AND 10485760),
    name TEXT NOT NULL CHECK(length(name)>0),
    expires_at INTEGER NOT NULL CHECK(expires_at BETWEEN 0 AND 9007199254740991),
    PRIMARY KEY(chat_id,id)
) WITHOUT ROWID;
CREATE INDEX attachment_reservations_expiry ON attachment_reservations(expires_at);
CREATE TRIGGER attachments_added AFTER INSERT ON attachments BEGIN
    UPDATE attachment_quotas SET committed_bytes=committed_bytes+NEW.size WHERE chat_id=NEW.chat_id;
END;
CREATE TRIGGER attachments_removed AFTER DELETE ON attachments BEGIN
    UPDATE attachment_quotas SET committed_bytes=committed_bytes-OLD.size WHERE chat_id=OLD.chat_id;
END;
CREATE TRIGGER attachment_reserved AFTER INSERT ON attachment_reservations BEGIN
    UPDATE attachment_quotas SET reserved_bytes=reserved_bytes+NEW.size WHERE chat_id=NEW.chat_id;
END;
CREATE TRIGGER attachment_released AFTER DELETE ON attachment_reservations BEGIN
    UPDATE attachment_quotas SET reserved_bytes=reserved_bytes-OLD.size WHERE chat_id=OLD.chat_id;
END;
CREATE TRIGGER attachments_immutable BEFORE UPDATE ON attachments BEGIN
    SELECT RAISE(ABORT,'Committed attachments are immutable');
END;
CREATE TRIGGER attachment_reservations_immutable BEFORE UPDATE OF chat_id,id,token,digest,size,name ON attachment_reservations BEGIN
    SELECT RAISE(ABORT,'Attachment reservation identity and bytes are immutable');
END;
