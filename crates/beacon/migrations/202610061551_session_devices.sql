-- A bounded, unverified user-agent description helps identify a lost device.
-- Do not retain network addresses or bearer credentials for this purpose.
ALTER TABLE web_sessions ADD COLUMN device text NOT NULL DEFAULT 'Unknown device';
ALTER TABLE web_sessions ADD CONSTRAINT web_sessions_device_length CHECK (length(device) <= 256);
