ALTER TABLE livestreamers ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1;
CREATE INDEX IF NOT EXISTS idx_livestreamers_enabled ON livestreamers(enabled);
