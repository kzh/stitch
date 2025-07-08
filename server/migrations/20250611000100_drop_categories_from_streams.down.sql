ALTER TABLE streams ADD COLUMN categories jsonb;
CREATE INDEX IF NOT EXISTS idx_streams_categories ON streams USING GIN(categories); 