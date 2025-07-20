use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::{postgres::PgPoolOptions, types::Json, PgPool};

pub(crate) type Pool = PgPool;

pub(crate) async fn establish_pool(database_url: &str) -> Result<Pool> {
    let pool = PgPoolOptions::new()
        .connect(database_url)
        .await
        .with_context(|| format!("connecting to database `{database_url}`"))?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("running migrations")?;
    Ok(pool)
}

pub(crate) async fn track_channel(
    pool: &Pool,
    channel: &str,
    display_name: &str,
    channel_id: &str,
) -> Result<Channel> {
    let now = Utc::now().naive_utc();
    let channel = sqlx::query_as::<_, Channel>(
        r#"
        INSERT INTO channels (name, display_name, channel_id, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (name) DO UPDATE SET updated_at = EXCLUDED.updated_at
        RETURNING id, name, display_name, channel_id, created_at, updated_at
        "#,
    )
    .bind(channel)
    .bind(display_name)
    .bind(channel_id)
    .bind(now)
    .bind(now)
    .fetch_one(pool)
    .await
    .with_context(|| format!("tracking channel `{channel}`"))?;
    Ok(channel)
}

pub(crate) async fn untrack_channel(pool: &Pool, channel: &str) -> Result<()> {
    sqlx::query(
        r#"
        DELETE FROM channels WHERE name = $1
        "#,
    )
    .bind(channel)
    .execute(pool)
    .await
    .with_context(|| format!("untracking channel `{channel}`"))?;
    Ok(())
}

#[derive(sqlx::FromRow, Serialize, Deserialize, Debug, Clone)]
pub(crate) struct Channel {
    pub id: i32,
    pub name: String,
    pub display_name: String,
    pub channel_id: String,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

pub(crate) async fn list_channels(pool: &Pool) -> Result<Vec<Channel>> {
    let channels = sqlx::query_as::<_, Channel>(
        r#"
        SELECT id, name, display_name, channel_id, created_at, updated_at FROM channels
        "#,
    )
    .fetch_all(pool)
    .await
    .context("listing channels")?;
    Ok(channels)
}

pub(crate) async fn get_channel_by_name(pool: &Pool, name: &str) -> Result<Channel> {
    let channel = sqlx::query_as::<_, Channel>(
        r#"
        SELECT id, name, display_name, channel_id, created_at, updated_at
          FROM channels WHERE name = $1
        "#,
    )
    .bind(name)
    .fetch_one(pool)
    .await
    .with_context(|| format!("getting channel by name `{name}`"))?;
    Ok(channel)
}

pub(crate) async fn update_channel(
    pool: &Pool,
    channel_id: &str,
    name: &str,
    display_name: &str,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE channels SET name = $1, display_name = $2 WHERE channel_id = $3
        "#,
    )
    .bind(name)
    .bind(display_name)
    .bind(channel_id)
    .execute(pool)
    .await
    .with_context(|| format!("updating channel `{channel_id}`"))?;
    Ok(())
}

pub(crate) async fn start_stream(
    pool: &Pool,
    stream_id: &str,
    channel_id: &str,
    title: &str,
    category: &str,
    message_id: u64,
    timestamp: chrono::DateTime<Utc>,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO streams (stream_id, channel_id, title, started_at, last_updated, message_id, events)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(stream_id)
    .bind(channel_id)
    .bind(title)
    .bind(timestamp)
    .bind(timestamp)
    .bind(message_id as i64)
    .bind(Json(vec![UpdateEvent {
        title: title.to_string(),
        category: category.to_string(),
        timestamp,
    }]))
    .execute(pool)
    .await
    .with_context(|| format!("starting stream `{stream_id}`"))?;
    Ok(())
}

pub(crate) async fn update_stream(
    pool: &Pool,
    stream_id: &str,
    title: &str,
    event: &UpdateEvent,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE streams
        SET title = $1, events = events || $2::jsonb
        WHERE stream_id = $3
        "#,
    )
    .bind(title)
    .bind(Json(event))
    .bind(stream_id)
    .execute(pool)
    .await
    .with_context(|| format!("updating stream `{stream_id}`"))?;
    Ok(())
}

pub(crate) async fn end_stream(
    pool: &Pool,
    stream_id: &str,
    title: &str,
    ended_at: chrono::DateTime<Utc>,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE streams
        SET ended_at = $1, title = $2
        WHERE stream_id = $3 AND ended_at IS NULL
        "#,
    )
    .bind(ended_at)
    .bind(title)
    .bind(stream_id)
    .execute(pool)
    .await
    .with_context(|| format!("ending stream `{stream_id}`"))?;
    Ok(())
}

#[derive(sqlx::FromRow, Serialize, Deserialize, Debug, Clone)]
pub struct UpdateEvent {
    pub title: String,
    pub category: String,
    pub timestamp: chrono::DateTime<Utc>,
}

#[derive(sqlx::FromRow, Serialize, Deserialize, Debug, Clone)]
pub struct Stream {
    pub id: i32,
    pub channel_id: String,
    pub stream_id: String,
    pub title: String,
    pub started_at: chrono::DateTime<Utc>,
    pub last_updated: chrono::DateTime<Utc>,
    pub message_id: i64,
    pub ended_at: Option<chrono::DateTime<Utc>>,
    pub events: Json<Vec<UpdateEvent>>,
}

pub(crate) async fn get_streams(pool: &Pool, channel_id: Option<String>) -> Result<Vec<Stream>> {
    let streams = sqlx::query_as::<_, Stream>(
        r#"
        SELECT id, channel_id, stream_id, title, started_at, ended_at, last_updated, message_id, events
        FROM streams
        WHERE channel_id = $1 OR ($1 IS NULL AND ended_at IS NULL)
        ORDER BY last_updated DESC
        "#,
    )
    .bind(channel_id)
    .fetch_all(pool)
    .await
    .context("getting streams")?;
    Ok(streams)
}
