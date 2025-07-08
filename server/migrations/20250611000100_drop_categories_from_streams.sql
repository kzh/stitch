ALTER TABLE streams DROP COLUMN IF EXISTS categories;
DROP INDEX IF EXISTS idx_streams_categories; 