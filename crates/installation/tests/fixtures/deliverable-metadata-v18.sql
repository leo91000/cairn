-- Real Store b3ebdc4b: upgraded v17 fixture, metadata written by its browser_fixture. All contents and credentials synthetic.
PRAGMA foreign_keys=OFF;
BEGIN TRANSACTION;
CREATE TABLE account_dismissals(
    account_id TEXT NOT NULL CHECK(length(account_id) BETWEEN 1 AND 200),
    task_id TEXT NOT NULL CHECK(length(task_id) BETWEEN 32 AND 45),
    finished_at INTEGER NOT NULL CHECK(finished_at BETWEEN 1 AND 9007199254740991),
    snoozed_until INTEGER CHECK(snoozed_until BETWEEN finished_at + 1 AND 9007199254740991),
    PRIMARY KEY(account_id,task_id)
) WITHOUT ROWID;
INSERT INTO "account_dismissals" VALUES('00000000-0000-4000-8000-0000000000a2','2fd1fcc6-d2d1-4c20-af8c-fecdac6c5578',1761800000000,1795077327056);
INSERT INTO "account_dismissals" VALUES('fixture-owner','2fd1fcc6-d2d1-4c20-af8c-fecdac6c5578',1761800000000,NULL);
INSERT INTO "account_dismissals" VALUES('fixture-owner','af845025-26ed-4d44-9617-8698c8619638',1761800001000,1795077327056);
CREATE TABLE account_exhaustion(
 account_id TEXT PRIMARY KEY NOT NULL REFERENCES coding_agent_accounts(id) ON DELETE CASCADE,
 at INTEGER NOT NULL CHECK(at BETWEEN 0 AND 9007199254740991),
 model TEXT NOT NULL
) WITHOUT ROWID;
INSERT INTO "account_exhaustion" VALUES('00000000-0000-4000-8000-000000000001',1700000000120,'gpt-spark');
CREATE TABLE account_leases(
 run_id TEXT PRIMARY KEY NOT NULL,
 account_id TEXT NOT NULL REFERENCES coding_agent_accounts(id) ON DELETE RESTRICT,
 provider TEXT NOT NULL CHECK(provider IN ('codex','claude','opencode')),
 model TEXT NOT NULL, home TEXT NOT NULL
);
INSERT INTO "account_leases" VALUES('preserved-background-lease','00000000-0000-4000-8000-000000000003','opencode','fixture-model','/synthetic/credential-home');
CREATE TABLE account_read_starts(
    account_id TEXT PRIMARY KEY NOT NULL CHECK(length(account_id) BETWEEN 1 AND 200),
    started_at INTEGER NOT NULL CHECK(started_at BETWEEN 0 AND 9007199254740991)
) WITHOUT ROWID;
INSERT INTO "account_read_starts" VALUES('00000000-0000-4000-8000-0000000000a2',1793349324007);
INSERT INTO "account_read_starts" VALUES('00000000-0000-4000-8000-0000000000a3',1793349324071);
INSERT INTO "account_read_starts" VALUES('fixture-owner',1793349323942);
CREATE TABLE account_reads(
    account_id TEXT NOT NULL CHECK(length(account_id) BETWEEN 1 AND 200),
    kind TEXT NOT NULL CHECK(kind IN ('chat','mission')),
    item_id TEXT NOT NULL CHECK(length(item_id) BETWEEN 32 AND 45),
    finished_at INTEGER NOT NULL CHECK(finished_at BETWEEN 0 AND 9007199254740991),
    PRIMARY KEY(account_id,kind,item_id)
) WITHOUT ROWID;
INSERT INTO "account_reads" VALUES('00000000-0000-4000-8000-0000000000a2','chat','ec5a395f-3940-4b8d-9f57-1530018c717f',1761750000000);
INSERT INTO "account_reads" VALUES('fixture-owner','chat','25d7ac0e-c91e-4a7e-b1e1-ab177dc6e02d',1793349326083);
INSERT INTO "account_reads" VALUES('fixture-owner','chat','EC5A395F-3940-4B8D-9F57-1530018C717F',1761700000000);
INSERT INTO "account_reads" VALUES('fixture-owner','mission','00000000-0000-4000-8000-0000000000ff',1761600000000);
INSERT INTO "account_reads" VALUES('fixture-owner','mission','2fd1fcc6-d2d1-4c20-af8c-fecdac6c5578',1761800000000);
CREATE TABLE account_usage(
 account_id TEXT NOT NULL REFERENCES coding_agent_accounts(id) ON DELETE CASCADE,
 phase TEXT NOT NULL CHECK(phase IN ('current','exhausted')),
 allowed INTEGER NOT NULL CHECK(allowed IN (0,1)),
 checked_at INTEGER CHECK(checked_at BETWEEN 0 AND 9007199254740991),
 attempted_at INTEGER CHECK(attempted_at BETWEEN 0 AND 9007199254740991),
 error TEXT, use_balance INTEGER NOT NULL CHECK(use_balance IN (0,1)),
 resets_available INTEGER CHECK(resets_available BETWEEN 0 AND 9007199254740991),
 credits TEXT CHECK(credits IS NULL OR (json_valid(credits) AND json_type(credits)='array')),
 PRIMARY KEY(account_id,phase)
) WITHOUT ROWID;
INSERT INTO "account_usage" VALUES('00000000-0000-4000-8000-000000000001','current',1,1700000000123,1700000000124,'Retained poll failure',1,2,'[{"expiresAt":1900000000,"id":"synthetic-credit"}]');
INSERT INTO "account_usage" VALUES('00000000-0000-4000-8000-000000000001','exhausted',1,1700000000123,1700000000124,'Retained poll failure',1,2,'[{"expiresAt":1900000000,"id":"synthetic-credit"}]');
INSERT INTO "account_usage" VALUES('00000000-0000-4000-8000-000000000003','current',1,1700000000123,1700000000124,'Retained poll failure',1,2,'[{"expiresAt":1900000000,"id":"synthetic-credit"}]');
CREATE TABLE account_usage_windows(
 account_id TEXT NOT NULL, phase TEXT NOT NULL,
 position INTEGER NOT NULL CHECK(position>=0),
 id TEXT NOT NULL, label TEXT NOT NULL,
 used_percent TEXT CHECK(used_percent IS NULL OR (json_valid(used_percent) AND json_type(used_percent) IN ('integer','real'))),
 resets_at INTEGER CHECK(resets_at BETWEEN 0 AND 9007199254740991),
 duration_mins INTEGER,
 models TEXT NOT NULL CHECK(json_valid(models) AND json_type(models)='array'),
 reached INTEGER NOT NULL CHECK(reached IN (0,1)),
 PRIMARY KEY(account_id,phase,position),
 FOREIGN KEY(account_id,phase) REFERENCES account_usage(account_id,phase) ON DELETE CASCADE
) WITHOUT ROWID;
INSERT INTO "account_usage_windows" VALUES('00000000-0000-4000-8000-000000000001','current',0,'weekly','Weekly','87.123456789',1900000000,10080,'[]',0);
INSERT INTO "account_usage_windows" VALUES('00000000-0000-4000-8000-000000000001','current',1,'model','Model','100',1900000001,300,'["gpt-6.1-sol","gpt-spark"]',1);
INSERT INTO "account_usage_windows" VALUES('00000000-0000-4000-8000-000000000001','exhausted',0,'weekly','Weekly','87.123456789',1900000000,10080,'[]',0);
INSERT INTO "account_usage_windows" VALUES('00000000-0000-4000-8000-000000000001','exhausted',1,'model','Model','100',1900000001,300,'["gpt-6.1-sol","gpt-spark"]',1);
INSERT INTO "account_usage_windows" VALUES('00000000-0000-4000-8000-000000000003','current',0,'weekly','Weekly','87.123456789',1900000000,10080,'[]',0);
INSERT INTO "account_usage_windows" VALUES('00000000-0000-4000-8000-000000000003','current',1,'model','Model','100',1900000001,300,'["gpt-6.1-sol","gpt-spark"]',1);
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
INSERT INTO "agent_nodes" VALUES('00000000-0000-4000-8000-000000000001','00000000-0000-4000-8000-000000000002',0);
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
INSERT INTO "agent_permissions" VALUES('00000000-0000-4000-8000-000000000001','firecracker','yolo',0,0,1,1,0,1);
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
INSERT INTO "agents" VALUES('00000000-0000-4000-8000-000000000001','Main agent','codex','Your default agent, with access to every registered project, skill, and shared connection.','','high','','',0,'21166419-7f9d-4ae2-9b31-da23fa948883',1793335875061,1793335875062,NULL,NULL,NULL,NULL);
CREATE TABLE attachment_quotas(
    chat_id TEXT PRIMARY KEY NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    committed_bytes INTEGER NOT NULL DEFAULT 0 CHECK(committed_bytes >= 0),
    reserved_bytes INTEGER NOT NULL DEFAULT 0 CHECK(reserved_bytes >= 0),
    CHECK(committed_bytes + reserved_bytes <= 209715200)
) WITHOUT ROWID;
INSERT INTO "attachment_quotas" VALUES('25d7ac0e-c91e-4a7e-b1e1-ab177dc6e02d',24,0);
INSERT INTO "attachment_quotas" VALUES('ec5a395f-3940-4b8d-9f57-1530018c717f',209715200,0);
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
INSERT INTO "attachments" VALUES('ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDE2','25d7ac0e-c91e-4a7e-b1e1-ab177dc6e02d','screenshot.gif',10,'image/gif','image','a39ba6aebc26432b0b646b0ca5b569fa7c114063f8afaf052744f1554d28cca8',1793335875101,2,3,'ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDE2');
INSERT INTO "attachments" VALUES('ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDE3','25d7ac0e-c91e-4a7e-b1e1-ab177dc6e02d','notes.txt',14,'application/octet-stream','file','9f4d6c783f3860390ce68a0ee314128e448725f938555ce10a151a0623bcf778',1793335875138,NULL,NULL,'ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDE3');
INSERT INTO "attachments" VALUES('00000000-0000-4000-8000-000000000001','ec5a395f-3940-4b8d-9f57-1530018c717f','ten-megabytes.txt',10485760,'application/octet-stream','file','462a12a876c0364e4f1f3d12ed33dcae125f1198010ff78d8f4c3f4de0412d49',1793335875290,NULL,NULL,'00000000-0000-4000-8000-000000000001');
INSERT INTO "attachments" VALUES('00000000-0000-4000-8000-000000000002','ec5a395f-3940-4b8d-9f57-1530018c717f','ten-megabytes.txt',10485760,'application/octet-stream','file','462a12a876c0364e4f1f3d12ed33dcae125f1198010ff78d8f4c3f4de0412d49',1793335875493,NULL,NULL,'00000000-0000-4000-8000-000000000002');
INSERT INTO "attachments" VALUES('00000000-0000-4000-8000-000000000003','ec5a395f-3940-4b8d-9f57-1530018c717f','ten-megabytes.txt',10485760,'application/octet-stream','file','462a12a876c0364e4f1f3d12ed33dcae125f1198010ff78d8f4c3f4de0412d49',1793335876157,NULL,NULL,'00000000-0000-4000-8000-000000000003');
INSERT INTO "attachments" VALUES('00000000-0000-4000-8000-000000000004','ec5a395f-3940-4b8d-9f57-1530018c717f','ten-megabytes.txt',10485760,'application/octet-stream','file','462a12a876c0364e4f1f3d12ed33dcae125f1198010ff78d8f4c3f4de0412d49',1793335876368,NULL,NULL,'00000000-0000-4000-8000-000000000004');
INSERT INTO "attachments" VALUES('00000000-0000-4000-8000-000000000005','ec5a395f-3940-4b8d-9f57-1530018c717f','ten-megabytes.txt',10485760,'application/octet-stream','file','462a12a876c0364e4f1f3d12ed33dcae125f1198010ff78d8f4c3f4de0412d49',1793335876606,NULL,NULL,'00000000-0000-4000-8000-000000000005');
INSERT INTO "attachments" VALUES('00000000-0000-4000-8000-000000000006','ec5a395f-3940-4b8d-9f57-1530018c717f','ten-megabytes.txt',10485760,'application/octet-stream','file','462a12a876c0364e4f1f3d12ed33dcae125f1198010ff78d8f4c3f4de0412d49',1793335877238,NULL,NULL,'00000000-0000-4000-8000-000000000006');
INSERT INTO "attachments" VALUES('00000000-0000-4000-8000-000000000007','ec5a395f-3940-4b8d-9f57-1530018c717f','ten-megabytes.txt',10485760,'application/octet-stream','file','462a12a876c0364e4f1f3d12ed33dcae125f1198010ff78d8f4c3f4de0412d49',1793335877474,NULL,NULL,'00000000-0000-4000-8000-000000000007');
INSERT INTO "attachments" VALUES('00000000-0000-4000-8000-000000000008','ec5a395f-3940-4b8d-9f57-1530018c717f','ten-megabytes.txt',10485760,'application/octet-stream','file','462a12a876c0364e4f1f3d12ed33dcae125f1198010ff78d8f4c3f4de0412d49',1793335877722,NULL,NULL,'00000000-0000-4000-8000-000000000008');
INSERT INTO "attachments" VALUES('00000000-0000-4000-8000-000000000009','ec5a395f-3940-4b8d-9f57-1530018c717f','ten-megabytes.txt',10485760,'application/octet-stream','file','462a12a876c0364e4f1f3d12ed33dcae125f1198010ff78d8f4c3f4de0412d49',1793335877938,NULL,NULL,'00000000-0000-4000-8000-000000000009');
INSERT INTO "attachments" VALUES('00000000-0000-4000-8000-00000000000a','ec5a395f-3940-4b8d-9f57-1530018c717f','ten-megabytes.txt',10485760,'application/octet-stream','file','462a12a876c0364e4f1f3d12ed33dcae125f1198010ff78d8f4c3f4de0412d49',1793335878238,NULL,NULL,'00000000-0000-4000-8000-00000000000a');
INSERT INTO "attachments" VALUES('00000000-0000-4000-8000-00000000000b','ec5a395f-3940-4b8d-9f57-1530018c717f','ten-megabytes.txt',10485760,'application/octet-stream','file','462a12a876c0364e4f1f3d12ed33dcae125f1198010ff78d8f4c3f4de0412d49',1793335878448,NULL,NULL,'00000000-0000-4000-8000-00000000000b');
INSERT INTO "attachments" VALUES('00000000-0000-4000-8000-00000000000c','ec5a395f-3940-4b8d-9f57-1530018c717f','ten-megabytes.txt',10485760,'application/octet-stream','file','462a12a876c0364e4f1f3d12ed33dcae125f1198010ff78d8f4c3f4de0412d49',1793335878657,NULL,NULL,'00000000-0000-4000-8000-00000000000c');
INSERT INTO "attachments" VALUES('00000000-0000-4000-8000-00000000000d','ec5a395f-3940-4b8d-9f57-1530018c717f','ten-megabytes.txt',10485760,'application/octet-stream','file','462a12a876c0364e4f1f3d12ed33dcae125f1198010ff78d8f4c3f4de0412d49',1793335878910,NULL,NULL,'00000000-0000-4000-8000-00000000000d');
INSERT INTO "attachments" VALUES('00000000-0000-4000-8000-00000000000e','ec5a395f-3940-4b8d-9f57-1530018c717f','ten-megabytes.txt',10485760,'application/octet-stream','file','462a12a876c0364e4f1f3d12ed33dcae125f1198010ff78d8f4c3f4de0412d49',1793335879121,NULL,NULL,'00000000-0000-4000-8000-00000000000e');
INSERT INTO "attachments" VALUES('00000000-0000-4000-8000-00000000000f','ec5a395f-3940-4b8d-9f57-1530018c717f','ten-megabytes.txt',10485760,'application/octet-stream','file','462a12a876c0364e4f1f3d12ed33dcae125f1198010ff78d8f4c3f4de0412d49',1793335879457,NULL,NULL,'00000000-0000-4000-8000-00000000000f');
INSERT INTO "attachments" VALUES('00000000-0000-4000-8000-000000000010','ec5a395f-3940-4b8d-9f57-1530018c717f','ten-megabytes.txt',10485760,'application/octet-stream','file','462a12a876c0364e4f1f3d12ed33dcae125f1198010ff78d8f4c3f4de0412d49',1793335879670,NULL,NULL,'00000000-0000-4000-8000-000000000010');
INSERT INTO "attachments" VALUES('00000000-0000-4000-8000-000000000011','ec5a395f-3940-4b8d-9f57-1530018c717f','ten-megabytes.txt',10485760,'application/octet-stream','file','462a12a876c0364e4f1f3d12ed33dcae125f1198010ff78d8f4c3f4de0412d49',1793335879912,NULL,NULL,'00000000-0000-4000-8000-000000000011');
INSERT INTO "attachments" VALUES('00000000-0000-4000-8000-000000000012','ec5a395f-3940-4b8d-9f57-1530018c717f','ten-megabytes.txt',10485760,'application/octet-stream','file','462a12a876c0364e4f1f3d12ed33dcae125f1198010ff78d8f4c3f4de0412d49',1793335880119,NULL,NULL,'00000000-0000-4000-8000-000000000012');
INSERT INTO "attachments" VALUES('00000000-0000-4000-8000-000000000013','ec5a395f-3940-4b8d-9f57-1530018c717f','ten-megabytes.txt',10485760,'application/octet-stream','file','462a12a876c0364e4f1f3d12ed33dcae125f1198010ff78d8f4c3f4de0412d49',1793335880421,NULL,NULL,'00000000-0000-4000-8000-000000000013');
INSERT INTO "attachments" VALUES('00000000-0000-4000-8000-000000000014','ec5a395f-3940-4b8d-9f57-1530018c717f','ten-megabytes.txt',10485760,'application/octet-stream','file','462a12a876c0364e4f1f3d12ed33dcae125f1198010ff78d8f4c3f4de0412d49',1793335880623,NULL,NULL,'00000000-0000-4000-8000-000000000014');
CREATE TABLE audit(id INTEGER PRIMARY KEY AUTOINCREMENT,created_at INTEGER NOT NULL,action TEXT NOT NULL,detail TEXT NOT NULL);
INSERT INTO "audit" VALUES(777,42,'probe','{}');
INSERT INTO "audit" VALUES(778,1793349324156,'task.saved','{"id":"2fd1fcc6-d2d1-4c20-af8c-fecdac6c5578"}');
INSERT INTO "audit" VALUES(779,1793349324217,'task.saved','{"id":"af845025-26ed-4d44-9617-8698c8619638"}');
INSERT INTO "audit" VALUES(780,1793349324288,'task.saved','{"id":"7fa07f2a-2bff-464a-bc93-ec72e7306769"}');
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
    question_id TEXT);
INSERT INTO "chat_messages" VALUES('ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDE2','00000000-0000-4000-8000-000000000071','12345678-1234-4234-8234-123456789002','Questionnaire response','','','','','steer','queued',99,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa');
INSERT INTO "chat_messages" VALUES('ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDE1','00000000-0000-4000-8000-000000000071','12345678-1234-4234-8234-123456789002','Questionnaire response','','','','','steer','queued',99,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa');
CREATE TABLE chat_questions(
    key INTEGER PRIMARY KEY,
    id TEXT NOT NULL UNIQUE CHECK(length(id)>0),
    chat_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    run_id TEXT NOT NULL,
    blocking INTEGER NOT NULL CHECK(blocking IN (0,1)),
    source TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('pending','answering','answered','cancelled')),
    created_at INTEGER CHECK(created_at BETWEEN 0 AND 9007199254740991),
    message_id TEXT
);
INSERT INTO "chat_questions" VALUES(1,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','00000000-0000-4000-8000-000000000071','historical-run',0,'cairn','answering',99,'ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDE1');
CREATE TABLE coding_agent_accounts(
 id TEXT PRIMARY KEY NOT NULL,
 provider TEXT NOT NULL CHECK(provider IN ('codex','claude','opencode')),
 name TEXT NOT NULL CHECK(length(trim(name)) BETWEEN 1 AND 100),
 enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),
 email TEXT, plan TEXT, identity TEXT,
 created_at INTEGER CHECK(created_at BETWEEN 0 AND 9007199254740991),
 checked_at INTEGER CHECK(checked_at BETWEEN 0 AND 9007199254740991),
 state TEXT NOT NULL CHECK(state IN ('pending','ready','error')),
 error TEXT NOT NULL,
 last_used_at INTEGER CHECK(last_used_at BETWEEN 0 AND 9007199254740991),
 max_concurrent_runs INTEGER CHECK(max_concurrent_runs BETWEEN 1 AND 9007199254740991),
 reset_error TEXT,
 priority INTEGER CHECK(priority BETWEEN 0 AND 9007199254740991),
 model_provider TEXT, model_provider_name TEXT,
 revision TEXT NOT NULL,
 updated_at INTEGER NOT NULL
);
INSERT INTO "coding_agent_accounts" VALUES('00000000-0000-4000-8000-000000000001','codex','Retained Codex',1,'account0@example.test','Retained plan','synthetic-identity-0',1700000000000,1700000000123,'ready','',1700000000099,7,'Retained reset status',2,NULL,NULL,'03b07b18-3238-400c-b088-9ea2e51a7324',1793252225652);
INSERT INTO "coding_agent_accounts" VALUES('00000000-0000-4000-8000-000000000002','claude','Retained Claude',0,'account1@example.test','Retained plan','synthetic-identity-1',1700000000001,1700000000123,'ready','',1700000000099,7,'Retained reset status',1,NULL,NULL,'2daa1bfd-2e15-46a9-aeb0-4cd78d0fa43c',1793252225757);
INSERT INTO "coding_agent_accounts" VALUES('00000000-0000-4000-8000-000000000003','opencode','Retained Opencode',1,'account2@example.test','Retained plan','synthetic-identity-2',1700000000002,1700000000123,'ready','',1700000000099,7,'Retained reset status',0,'opencode-go','OpenCode Go','af3bc799-820a-42c0-be49-7100ae179ab0',1793252225866);
CREATE TABLE conversation_engine_switches(
    chat_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    request_id TEXT NOT NULL,
    run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    turn_id TEXT NOT NULL CHECK(length(turn_id) BETWEEN 1 AND 200),
    provider TEXT NOT NULL CHECK(provider IN ('codex','claude','opencode')),
    model TEXT NOT NULL,
    pending INTEGER NOT NULL CHECK(pending IN (0,1)),
    error TEXT,
    retry_at INTEGER,
    receipt_expires_at INTEGER NOT NULL,
    current_expires_at INTEGER,
    is_current INTEGER NOT NULL CHECK(is_current IN (0,1)),
    revision TEXT NOT NULL,
    PRIMARY KEY(chat_id,request_id)
) WITHOUT ROWID;
CREATE TABLE conversation_lineage(
    chat_id TEXT PRIMARY KEY NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    requester_chat_id TEXT NOT NULL,
    requester_run_id TEXT NOT NULL,
    request_id TEXT
) WITHOUT ROWID;
INSERT INTO "conversation_lineage" VALUES('retained-chat','00000000-0000-4000-8000-000000000071','historical-run','fixture-request');
CREATE TABLE conversation_retention(singleton INTEGER PRIMARY KEY CHECK(singleton=1),inactivity_days INTEGER NOT NULL CHECK(inactivity_days BETWEEN 0 AND 36500));
INSERT INTO "conversation_retention" VALUES(1,19);
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
INSERT INTO "conversations" VALUES('00000000-0000-4000-8000-000000000071','soft-otter-72c9d51b','Retained v14 conversation','deleted-agent',NULL,NULL,NULL,NULL,1,'active',42,51,NULL,'73704d8a-9f00-4eb6-8e89-e61af0a961f7',NULL,NULL,NULL,NULL,NULL,NULL,0,0,NULL,NULL,'["member-b","owner-a"]','["deleted-node"]',NULL,NULL,0,NULL,NULL,NULL,NULL,NULL,'00000000-0000-4000-8000-000000000003',1793313446556);
INSERT INTO "conversations" VALUES('retained-chat','little-moon-480bd6dd','','',NULL,NULL,NULL,NULL,0,'active',0,0,NULL,'3619f774-eaef-4183-aac8-8e93d73cafe7',NULL,NULL,NULL,NULL,NULL,NULL,0,0,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'00000000-0000-4000-8000-000000000001',1793313446556);
INSERT INTO "conversations" VALUES('25d7ac0e-c91e-4a7e-b1e1-ab177dc6e02d','merry-maple-b29ec8d3','New chat','00000000-0000-4000-8000-000000000001','00000000-0000-4000-8000-000000000001',NULL,NULL,NULL,0,'active',1793335875080,1793335875080,1793335875080,'90677c64-17d9-4660-b4a2-3619feb86216',NULL,NULL,NULL,NULL,NULL,NULL,0,0,NULL,NULL,'[]','[]',NULL,NULL,1,'[]',NULL,NULL,NULL,NULL,NULL,1793335875082);
INSERT INTO "conversations" VALUES('ec5a395f-3940-4b8d-9f57-1530018c717f','lucky-cedar-73f54470','New chat','00000000-0000-4000-8000-000000000001','00000000-0000-4000-8000-000000000001',NULL,NULL,NULL,0,'active',1793335875163,1793335875163,1793335875163,'efdaf1c3-3f21-4de2-a744-50ad2fdbd8e6',NULL,NULL,NULL,NULL,NULL,NULL,0,0,NULL,NULL,'[]','[]',NULL,NULL,1,'[]',NULL,NULL,NULL,NULL,NULL,1793335875163);
CREATE TABLE direct_journal_acknowledgements(publication TEXT PRIMARY KEY REFERENCES shared_publications(id),run_id TEXT NOT NULL UNIQUE,node_id TEXT NOT NULL,retired INTEGER NOT NULL DEFAULT 0 CHECK(retired IN (0,1)));
INSERT INTO "direct_journal_acknowledgements" VALUES('synthetic-publication','retained-run','synthetic-node',1);
CREATE TABLE direct_object_quarantines(object TEXT PRIMARY KEY);
INSERT INTO "direct_object_quarantines" VALUES('synthetic-object');
CREATE TABLE direct_read_authorizations(grant_id TEXT NOT NULL,object TEXT NOT NULL,hash TEXT NOT NULL,run_id TEXT NOT NULL,PRIMARY KEY(grant_id,object));
INSERT INTO "direct_read_authorizations" VALUES('synthetic-grant','synthetic-object','synthetic-hash','retained-run');
CREATE TABLE direct_storage_grants(object TEXT PRIMARY KEY REFERENCES shared_objects(id) ON DELETE CASCADE,expires_at INTEGER NOT NULL,settled INTEGER NOT NULL DEFAULT 0 CHECK(settled IN (0,1)));
INSERT INTO "direct_storage_grants" VALUES('synthetic-object',1900000000000,0);
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
INSERT INTO "durable_wakeups" VALUES(1,'00000000-0000-4000-8000-000000000071',NULL,'wake-v15','turn-v15','synthetic grouped upgrade fixture',3600,123,3600123,'all',NULL,NULL,NULL,NULL,NULL,NULL,NULL);
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
CREATE TABLE "kv"(key TEXT PRIMARY KEY CHECK(key NOT LIKE 'run-checkpoint:%' AND key NOT LIKE 'chat-coding-agent:%' AND key NOT LIKE 'chat-coding-agent-request:%' AND key NOT LIKE 'chat-question:%' AND key NOT LIKE 'chat-attachment:%' AND key!='conversation-expiration' AND key NOT LIKE 'read-since:%' AND key NOT LIKE 'read:%' AND key NOT LIKE 'dismissed:%'),data TEXT NOT NULL,expires INTEGER);
INSERT INTO "kv" VALUES('mcp-secret:synthetic:00000000-0000-4000-8000-000000000001','"Hv8FfBA7+0hq/FGjlRSiVOHO/eODsBNrUe91SqPb0A+r7gm7qOGLY4xPq7dCkYrJjvaUMST3TKlXXJ60SrB2hCs="',NULL);
INSERT INTO "kv" VALUES('mcp-secret:synthetic:00000000-0000-4000-8000-000000000002','"Al553tpQZAdZsNT3EUBEESEKlOXGZoYQD8nVw+mX3l3T7t6Az9CIruZ4QjDpAWwY0jK49Y8if6KOT+bkhlSSIjE="',NULL);
INSERT INTO "kv" VALUES('mcp-secret:synthetic:00000000-0000-4000-8000-000000000003','"7JKXWaryh3L6Qo6gcu1lEPv3yZ0z6h+aB3gBKECWr9ElMRVmI9GACUdlghcji8B4sFHipFB3JgFBR8Qu/5sxXRQ="',NULL);
INSERT INTO "kv" VALUES('synthetic-expiry','{"counter":9007199254740001}',9999999999999);
INSERT INTO "kv" VALUES('counter:probe','9007199254740001',NULL);
INSERT INTO "kv" VALUES('d2-fixture-proof','{"chat":"25d7ac0e-c91e-4a7e-b1e1-ab177dc6e02d","fullChat":"ec5a395f-3940-4b8d-9f57-1530018c717f","source":"b9cd3c2a"}',NULL);
INSERT INTO "kv" VALUES('test-relay:GMZdmMpHEiUchRmlc0kLxfwLTY6eAqHJJ4Dqwt3QbBg','{}',NULL);
INSERT INTO "kv" VALUES('test-relay:ivT4IHp3jtOsWqPSXoN6WrLqDTyaWyWlIj2Rq7WCVXY','{"account":"00000000-0000-4000-8000-0000000000a2"}',NULL);
INSERT INTO "kv" VALUES('test-relay:KcsJuGT4iD9JyoTnoXRekF2MrKLzSsgBy8DMhE117aA','{"account":"00000000-0000-4000-8000-0000000000a3"}',NULL);
INSERT INTO "kv" VALUES('b4-fixture-proof','{"chat":"25d7ac0e-c91e-4a7e-b1e1-ab177dc6e02d","fullChat":"ec5a395f-3940-4b8d-9f57-1530018c717f","missions":["2fd1fcc6-d2d1-4c20-af8c-fecdac6c5578","af845025-26ed-4d44-9617-8698c8619638","7fa07f2a-2bff-464a-bc93-ec72e7306769"],"runs":["00000000-0000-4000-8000-0000000000c0","00000000-0000-4000-8000-0000000000c1","00000000-0000-4000-8000-0000000000c2"],"source":"f489e1a5","until":1795077327056}',NULL);
INSERT INTO "kv" VALUES('artifact:00000000-0000-4000-8000-000000000010:00000000-0000-4000-8000-000000000d01','{"createdAt":1700000000001,"digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","excerpt":"Texte de synthèse","group":"review","id":"00000000-0000-4000-8000-000000000d01","key":"report","kind":"markdown","mediaType":"text/markdown","messageId":null,"name":"report.md","previewStatus":"none","publicToken":"00000000-0000-4000-8000-000000000e01","publicUrl":"https://example.invalid/artifacts/00000000-0000-4000-8000-000000000e01","runId":"00000000-0000-4000-8000-000000000010","size":18,"title":"Rapport été 1","url":"/api/runs/00000000-0000-4000-8000-000000000010/artifacts/00000000-0000-4000-8000-000000000d01","version":1,"visibility":"public"}',NULL);
INSERT INTO "kv" VALUES('artifact-share:00000000-0000-4000-8000-000000000e01','{"id":"00000000-0000-4000-8000-000000000d01","runId":"00000000-0000-4000-8000-000000000010"}',NULL);
INSERT INTO "kv" VALUES('artifact:00000000-0000-4000-8000-000000000010:00000000-0000-4000-8000-000000000d02','{"createdAt":1700000000002,"digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","excerpt":"Texte de synthèse","group":"review","id":"00000000-0000-4000-8000-000000000d02","key":"report","kind":"markdown","mediaType":"text/markdown","messageId":null,"name":"report.md","previewStatus":"none","publicToken":null,"publicUrl":null,"runId":"00000000-0000-4000-8000-000000000010","size":18,"title":"Rapport été 2","url":"/api/runs/00000000-0000-4000-8000-000000000010/artifacts/00000000-0000-4000-8000-000000000d02","version":2,"visibility":"private"}',NULL);
INSERT INTO "kv" VALUES('artifact:00000000-0000-4000-8000-000000000010:00000000-0000-4000-8000-000000000d03','{"createdAt":1700000000003,"digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","excerpt":null,"group":"review","id":"00000000-0000-4000-8000-000000000d03","key":"picture","kind":"image","mediaType":"image/png","messageId":null,"name":"picture.png","previewStatus":"pending","publicToken":null,"publicUrl":null,"runId":"00000000-0000-4000-8000-000000000010","size":18,"title":"Rapport été 3","url":"/api/runs/00000000-0000-4000-8000-000000000010/artifacts/00000000-0000-4000-8000-000000000d03","version":1,"visibility":"private"}',NULL);
INSERT INTO "kv" VALUES('skill-source:global/guide','{"folder":"skills/guide","importedAt":1700000000000,"instance":"00000000-0000-4000-8000-000000000d99","label":"Example skill","location":{"ref":"main","repository":"example/skills"},"pluginId":"github","revision":"bundle-revision-1","source":"repository","url":"https://example.invalid/skills","version":"revision-1"}',NULL);
INSERT INTO "kv" VALUES('unrelated-a16-proof','{"unchanged":true}',NULL);
INSERT INTO "kv" VALUES('skill-source:00000000-0000-4000-8000-000000000dad/guide','{"folder":"skills/guide","importedAt":1700000000000,"instance":"00000000-0000-4000-8000-000000000d99","label":"Example skill","location":{"ref":"main","repository":"example/skills"},"pluginId":"github","revision":"bundle-revision-1","source":"repository","url":"https://example.invalid/skills","version":"revision-1"}',NULL);
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
INSERT INTO "message_attachments" VALUES('ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDE1',0,'b','00000000-0000-4000-8000-000000000071','B',0,'application/octet-stream','file',NULL,NULL,'',0);
INSERT INTO "message_attachments" VALUES('ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDE1',1,'a','00000000-0000-4000-8000-000000000071','A',0,'application/octet-stream','image',10,20,'',0);
INSERT INTO "message_attachments" VALUES('ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDE2',0,'b','00000000-0000-4000-8000-000000000071','B',0,'application/octet-stream','file',NULL,NULL,'',0);
INSERT INTO "message_attachments" VALUES('ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDE2',1,'a','00000000-0000-4000-8000-000000000071','A',0,'application/octet-stream','image',10,20,'',0);
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
INSERT INTO "missions" VALUES('2fd1fcc6-d2d1-4c20-af8c-fecdac6c5578','00000000-0000-4000-8000-000000000001',NULL,'Release notes','Synthetic',NULL,'Europe/Paris',1,0,1,1,NULL,'fixture-owner','fixture-owner',0,'84c68882-34ba-4686-909f-d30147752ff1',1793349324154,1793349324156,NULL,NULL,NULL,NULL,'[]','[]',NULL,NULL,1,NULL);
INSERT INTO "missions" VALUES('af845025-26ed-4d44-9617-8698c8619638','00000000-0000-4000-8000-000000000001',NULL,'Device proof','Synthetic',NULL,'Europe/Paris',1,0,1,1,NULL,'fixture-owner','fixture-owner',0,'e3632ac9-e0c0-4685-bace-26f616dd0024',1793349324217,1793349324217,NULL,NULL,NULL,NULL,'[]','[]',NULL,NULL,1,NULL);
INSERT INTO "missions" VALUES('7fa07f2a-2bff-464a-bc93-ec72e7306769','00000000-0000-4000-8000-000000000001',NULL,'Nightly review','Synthetic',NULL,'Europe/Paris',1,0,1,1,NULL,'fixture-owner','fixture-owner',0,'87cce787-dde5-496b-9395-04ecae78e2fa',1793349324288,1793349324288,NULL,NULL,NULL,NULL,'[]','[]',NULL,NULL,1,NULL);
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
CREATE TABLE question_answer_choices(
    message_id TEXT NOT NULL,
    field_id TEXT NOT NULL,
    position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 29),
    option_id TEXT NOT NULL,
    PRIMARY KEY(message_id,field_id,position),
    FOREIGN KEY(message_id,field_id) REFERENCES question_answers(message_id,field_id) ON DELETE CASCADE
) WITHOUT ROWID;
INSERT INTO "question_answer_choices" VALUES('ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDE1','choice',0,'b');
INSERT INTO "question_answer_choices" VALUES('ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDE1','choice',1,'a');
INSERT INTO "question_answer_choices" VALUES('ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDE2','choice',0,'b');
INSERT INTO "question_answer_choices" VALUES('ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDE2','choice',1,'a');
CREATE TABLE question_answer_deliveries(
    message_id TEXT PRIMARY KEY NOT NULL REFERENCES chat_messages(id) ON DELETE CASCADE,
    text TEXT NOT NULL,
    private INTEGER NOT NULL CHECK(private IN (0,1))
);
INSERT INTO "question_answer_deliveries" VALUES('ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDE2','Retained private answer',1);
INSERT INTO "question_answer_deliveries" VALUES('ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDE1','Retained private answer',1);
CREATE TABLE question_answers(
    message_id TEXT NOT NULL REFERENCES chat_messages(id) ON DELETE CASCADE,
    field_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('current','legacy')),
    comment TEXT,
    CHECK((kind='current' AND comment IS NOT NULL AND length(comment)<=10000)
       OR (kind='legacy' AND comment IS NULL)),
    PRIMARY KEY(message_id,field_id)
) WITHOUT ROWID;
INSERT INTO "question_answers" VALUES('ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDE1','choice','current','synthetic-private-answer');
INSERT INTO "question_answers" VALUES('ABCDEFAB-CDEF-4ABC-8DEF-ABCDEFABCDE2','choice','current','synthetic-private-answer');
CREATE TABLE question_fields(
    question_key INTEGER NOT NULL REFERENCES chat_questions(key) ON DELETE CASCADE,
    id TEXT NOT NULL CHECK(length(id) BETWEEN 1 AND 200),
    position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 19),
    title TEXT NOT NULL CHECK(length(title) BETWEEN 1 AND 10000),
    secret INTEGER NOT NULL CHECK(secret IN (0,1)),
    mode TEXT NOT NULL CHECK(mode IN ('single','multiple')),
    min_choices INTEGER CHECK(min_choices BETWEEN 1 AND 30),
    max_choices INTEGER CHECK(max_choices BETWEEN 1 AND 30),
    CHECK(min_choices IS NULL OR max_choices IS NULL OR min_choices<=max_choices),
    CHECK(mode!='single' OR max_choices IS NULL OR max_choices=1),
    PRIMARY KEY(question_key,id), UNIQUE(question_key,position)
) WITHOUT ROWID;
INSERT INTO "question_fields" VALUES(1,'field',0,'Synthetic private question',1,'multiple',NULL,NULL);
CREATE TABLE question_import_metadata(
    question_id TEXT PRIMARY KEY NOT NULL REFERENCES chat_questions(id) ON DELETE CASCADE,
    expires INTEGER,
    metadata TEXT NOT NULL CHECK(json_valid(metadata) AND json_type(metadata)='object')
) WITHOUT ROWID;
CREATE TABLE question_legacy_answer_items(
    message_id TEXT NOT NULL,
    field_id TEXT NOT NULL,
    position INTEGER NOT NULL CHECK(position>=0),
    text TEXT NOT NULL,
    PRIMARY KEY(message_id,field_id,position),
    FOREIGN KEY(message_id,field_id) REFERENCES question_answers(message_id,field_id) ON DELETE CASCADE
) WITHOUT ROWID;
CREATE TABLE question_options(
    question_key INTEGER NOT NULL,
    field_id TEXT NOT NULL,
    id TEXT NOT NULL CHECK(length(id)<=200),
    position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 29),
    label TEXT NOT NULL CHECK(length(label) BETWEEN 1 AND 2000),
    description TEXT NOT NULL CHECK(length(description)<=4000),
    recommended INTEGER NOT NULL CHECK(recommended IN (0,1)),
    PRIMARY KEY(question_key,field_id,position),
    FOREIGN KEY(question_key,field_id) REFERENCES question_fields(question_key,id) ON DELETE CASCADE
) WITHOUT ROWID;
INSERT INTO "question_options" VALUES(1,'field','second',0,'Second','retained order',1);
INSERT INTO "question_options" VALUES(1,'field','first',1,'First','',0);
CREATE TABLE "records"(id TEXT PRIMARY KEY,kind TEXT NOT NULL CHECK(kind NOT IN ('agentAccounts','chats','nodes','projects','agents','tasks','mcps','plugin-connections','plugin-packages','plugin-requests','plugin-activity','plugin-timers','plugin-events','plugin-deliveries','plugin-storage','plugin-http-receipts')),data TEXT NOT NULL,updated_at INTEGER NOT NULL);
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
INSERT INTO "runs" VALUES('00000000-0000-4000-8000-0000000000c0','2fd1fcc6-d2d1-4c20-af8c-fecdac6c5578',NULL,'succeeded','schedule',1761799999500,NULL,1761800000000,'## Done

Synthetic **summary**.',NULL,NULL,NULL,NULL,0,0,0,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'{"agent":{"access":{"backend":"firecracker","delegation":false,"installation":false,"mcpTools":{},"mcps":null,"nodes":["00000000-0000-4000-8000-000000000002"],"projects":null,"sandbox":"yolo","skills":null},"createdAt":0,"description":"","exclusiveNodes":[],"id":"00000000-0000-4000-8000-000000000001","instructions":"","model":"","name":"Fixture","nodePriorities":[],"plugins":[],"provider":"codex","reasoning":"high","revision":"fixture","speed":"","timeoutMinutes":0},"availableProjects":[],"delegation":null,"project":null,"projects":[],"skills":[],"task":{"agentId":"00000000-0000-4000-8000-000000000001","archived":false,"authorAccessId":null,"authorId":null,"authorRemoved":false,"createdAt":null,"cron":null,"enabled":true,"id":"00000000-0000-4000-8000-000000000003","name":"Fixture","nativeAuthorityNodes":[],"nativeParentAccess":null,"nativeParentRunId":null,"nativePluginGrants":null,"nativePrincipals":[],"nativeProvenanceUnknown":false,"nextRun":null,"projectId":null,"prompt":"Fixture","revision":null,"scheduleFailure":null,"scheduleWaitReason":null,"skills":null,"tags":[],"timezone":"Europe/Paris","worktree":true}}','{"outcome":{"reason":"Synthetic wait.","status":"blocked"}}',NULL);
INSERT INTO "runs" VALUES('00000000-0000-4000-8000-0000000000c1','af845025-26ed-4d44-9617-8698c8619638',NULL,'succeeded','schedule',1761800000500,NULL,1761800001000,'## Done

Synthetic **summary**.',NULL,NULL,NULL,NULL,0,0,0,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'{"agent":{"access":{"backend":"firecracker","delegation":false,"installation":false,"mcpTools":{},"mcps":null,"nodes":["00000000-0000-4000-8000-000000000002"],"projects":null,"sandbox":"yolo","skills":null},"createdAt":0,"description":"","exclusiveNodes":[],"id":"00000000-0000-4000-8000-000000000001","instructions":"","model":"","name":"Fixture","nodePriorities":[],"plugins":[],"provider":"codex","reasoning":"high","revision":"fixture","speed":"","timeoutMinutes":0},"availableProjects":[],"delegation":null,"project":null,"projects":[],"skills":[],"task":{"agentId":"00000000-0000-4000-8000-000000000001","archived":false,"authorAccessId":null,"authorId":null,"authorRemoved":false,"createdAt":null,"cron":null,"enabled":true,"id":"00000000-0000-4000-8000-000000000003","name":"Fixture","nativeAuthorityNodes":[],"nativeParentAccess":null,"nativeParentRunId":null,"nativePluginGrants":null,"nativePrincipals":[],"nativeProvenanceUnknown":false,"nextRun":null,"projectId":null,"prompt":"Fixture","revision":null,"scheduleFailure":null,"scheduleWaitReason":null,"skills":null,"tags":[],"timezone":"Europe/Paris","worktree":true}}','{"outcome":{"reason":"Synthetic wait.","status":"needs_input"}}',NULL);
INSERT INTO "runs" VALUES('00000000-0000-4000-8000-0000000000c2','7fa07f2a-2bff-464a-bc93-ec72e7306769',NULL,'succeeded','schedule',1761800001500,NULL,1761800002000,'## Done

Synthetic **summary**.',NULL,NULL,NULL,NULL,0,0,0,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'{"agent":{"access":{"backend":"firecracker","delegation":false,"installation":false,"mcpTools":{},"mcps":null,"nodes":["00000000-0000-4000-8000-000000000002"],"projects":null,"sandbox":"yolo","skills":null},"createdAt":0,"description":"","exclusiveNodes":[],"id":"00000000-0000-4000-8000-000000000001","instructions":"","model":"","name":"Fixture","nodePriorities":[],"plugins":[],"provider":"codex","reasoning":"high","revision":"fixture","speed":"","timeoutMinutes":0},"availableProjects":[],"delegation":null,"project":null,"projects":[],"skills":[],"task":{"agentId":"00000000-0000-4000-8000-000000000001","archived":false,"authorAccessId":null,"authorId":null,"authorRemoved":false,"createdAt":null,"cron":null,"enabled":true,"id":"00000000-0000-4000-8000-000000000003","name":"Fixture","nativeAuthorityNodes":[],"nativeParentAccess":null,"nativeParentRunId":null,"nativePluginGrants":null,"nativePrincipals":[],"nativeProvenanceUnknown":false,"nextRun":null,"projectId":null,"prompt":"Fixture","revision":null,"scheduleFailure":null,"scheduleWaitReason":null,"skills":null,"tags":[],"timezone":"Europe/Paris","worktree":true}}','{"outcome":{"reason":"Synthetic wait.","status":"blocked"}}',NULL);
CREATE TABLE shared_objects(id TEXT PRIMARY KEY,destination TEXT NOT NULL,hash TEXT NOT NULL,size INTEGER NOT NULL,location TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN ('pending','ready','invalid','deleting')),unused_at INTEGER,retry_at INTEGER NOT NULL DEFAULT 0);
INSERT INTO "shared_objects" VALUES('synthetic-object','synthetic-bucket','synthetic-hash',4096,'{}','ready',NULL,0);
CREATE TABLE shared_publications(id TEXT PRIMARY KEY,run_id TEXT NOT NULL);
INSERT INTO "shared_publications" VALUES('synthetic-publication','retained-run');
CREATE TABLE shared_references(publication TEXT NOT NULL REFERENCES shared_publications(id) ON DELETE CASCADE,object TEXT NOT NULL REFERENCES shared_objects(id),PRIMARY KEY(publication,object));
INSERT INTO "shared_references" VALUES('synthetic-publication','synthetic-object');
CREATE TABLE wait_conditions(
    wakeup_key INTEGER NOT NULL REFERENCES durable_wakeups(key) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK(position>=0),
    kind TEXT NOT NULL CHECK(kind IN ('run_finished','task_finished','plugin')),
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
       OR (kind='plugin' AND target_run_id IS NULL AND target_chat_id IS NULL AND plugin_id IS NOT NULL AND condition_type IS NOT NULL AND plugin_input IS NOT NULL)),
    PRIMARY KEY(wakeup_key,position)
) WITHOUT ROWID;
INSERT INTO "wait_conditions" VALUES(1,0,'plugin',NULL,NULL,'github','github_workflow','00000000-0000-4000-8000-000000000001','{"headSha":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","runId":123}',NULL,'{"title":"Build","short":"CI","details":["synthetic"]}',987,'retry synthetic');
INSERT INTO "wait_conditions" VALUES(1,1,'run_finished','historical-run',NULL,NULL,NULL,NULL,NULL,'{"state":"completed","type":"run_finished","runId":"historical-run","status":"failed","outcomeStatus":"blocked","waitingOn":"requester","finishedAt":456}',NULL,NULL,NULL);
CREATE TRIGGER missions_author_immutable BEFORE UPDATE ON missions
 WHEN OLD.author_id IS NOT NEW.author_id OR OLD.author_access_id IS NOT NEW.author_access_id
   OR (OLD.author_removed=1 AND NEW.author_removed=0)
 BEGIN SELECT RAISE(ABORT,'Mission authorship is immutable'); END;
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
CREATE INDEX events_run ON events(run_id,id);
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
CREATE INDEX missions_updated ON missions(updated_at DESC,id);
CREATE INDEX missions_due ON missions(next_run,id) WHERE enabled=1 AND archived=0 AND cron IS NOT NULL;
CREATE INDEX missions_agent ON missions(agent_id);
CREATE INDEX missions_project ON missions(project_id) WHERE project_id IS NOT NULL;
CREATE INDEX direct_read_authorizations_object ON direct_read_authorizations(object);
CREATE UNIQUE INDEX coding_agent_accounts_identity ON coding_agent_accounts(provider,identity) WHERE identity IS NOT NULL;
CREATE INDEX coding_agent_accounts_updated ON coding_agent_accounts(updated_at DESC,id);
CREATE INDEX account_leases_account ON account_leases(account_id,run_id);
CREATE UNIQUE INDEX conversations_run ON conversations(run_id) WHERE run_id IS NOT NULL;
CREATE INDEX conversations_live_agent ON conversations(live_agent_id) WHERE live_agent_id IS NOT NULL;
CREATE INDEX conversations_live_project ON conversations(live_project_id) WHERE live_project_id IS NOT NULL;
CREATE INDEX conversations_updated ON conversations(persisted_at DESC,id);
CREATE INDEX chat_messages_chat ON chat_messages(chat_id,created_at);
CREATE INDEX chat_messages_pending ON chat_messages(chat_id,status,created_at);
CREATE UNIQUE INDEX conversation_engine_switch_current ON conversation_engine_switches(chat_id) WHERE is_current=1;
CREATE INDEX records_kind ON records(kind,updated_at DESC);
CREATE INDEX records_node_backups_run ON records(json_extract(data,'$.runId')) WHERE kind='node-backups';
CREATE INDEX chat_questions_chat ON chat_questions(chat_id,created_at,id);
CREATE INDEX chat_questions_run ON chat_questions(run_id,status);
CREATE UNIQUE INDEX durable_wakeups_chat ON durable_wakeups(chat_id) WHERE chat_id IS NOT NULL;
CREATE UNIQUE INDEX durable_wakeups_mission ON durable_wakeups(mission_run_id) WHERE mission_run_id IS NOT NULL;
CREATE UNIQUE INDEX conversation_lineage_request ON conversation_lineage(requester_chat_id,request_id);
CREATE INDEX attachments_order ON attachments(chat_id,created_at,id);
CREATE INDEX attachment_reservations_expiry ON attachment_reservations(expires_at);
DELETE FROM "sqlite_sequence";
INSERT INTO "sqlite_sequence" VALUES('audit',780);
COMMIT;
PRAGMA user_version=18;
