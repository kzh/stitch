CREATE TABLE IF NOT EXISTS streams(
    id serial PRIMARY KEY,
    channel_id text NOT NULL REFERENCES channels(channel_id) ON DELETE CASCADE,
    stream_id text NOT NULL UNIQUE,
    title text NOT NULL,
    started_at timestamp with time zone NOT NULL,
    ended_at timestamp with time zone,
    last_updated timestamp with time zone NOT NULL,
    categories jsonb,
    message_id bigint NOT NULL,
    events jsonb NOT NULL DEFAULT '[]'
);

CREATE INDEX IF NOT EXISTS idx_streams_channel_id ON streams(channel_id);
CREATE INDEX IF NOT EXISTS idx_streams_started_at ON streams(started_at);
CREATE INDEX IF NOT EXISTS idx_streams_categories ON streams USING GIN(categories);
