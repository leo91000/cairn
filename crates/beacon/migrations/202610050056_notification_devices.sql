CREATE TABLE notification_devices (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES cairn_accounts(id) ON DELETE CASCADE,
    endpoint TEXT NOT NULL UNIQUE,
    p256dh TEXT NOT NULL,
    auth TEXT NOT NULL
);
CREATE INDEX notification_devices_account ON notification_devices(account_id);
