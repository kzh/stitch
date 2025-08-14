use crate::adapters::db;
use crate::adapters::twitch::TwitchStream;
use crate::utils::ttl_set;
use axum::{
    body::Bytes,
    extract::State,
    http::{header::HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing, Router,
};
use chrono::{DateTime, Utc};
use dashmap::{DashMap, Entry};
use futures::stream::{self, StreamExt};
use hex;
use hmac::{digest::Key, Hmac, Mac};
use serde::Deserialize;
use serenity::all::{EditMessage, MessageId};
use serenity::{
    all::{CreateEmbed, CreateMessage, Message},
    http::Http as DiscordHttp,
    model::{colour, id::ChannelId},
};
use sha2::Sha256;
use std::{cmp::Reverse, collections::hash_map::RandomState, future::Future};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;
use tracing::{error, info, instrument, warn};

const SIGNATURE_PREFIX: &str = "sha256=";
const WEBHOOK_VERIFICATION_TYPE: &str = "webhook_callback_verification";
const NOTIFICATION_TYPE: &str = "notification";
const MAX_TIMESTAMP_AGE_SECONDS: u64 = 600;
const MAX_FUTURE_TIMESTAMP_SECONDS: u64 = 180;

const HEADER_SIGNATURE: &str = "Twitch-Eventsub-Message-Signature";
const HEADER_TIMESTAMP: &str = "Twitch-Eventsub-Message-Timestamp";
const HEADER_MESSAGE_ID: &str = "Twitch-Eventsub-Message-Id";
const HEADER_MESSAGE_TYPE: &str = "Twitch-Eventsub-Message-Type";

const CONCURRENCY_LIMIT: usize = 40;

#[derive(thiserror::Error, Debug)]
pub enum WebhookError {
    #[error("Verification failed: {0}")]
    VerificationFailed(String),
    #[error("Duplicate message ID: {0}")]
    DuplicateMessageId(String),
    #[error("Bad payload: {0}")]
    BadPayload(String),
    #[error("Missing header: {0}")]
    MissingHeader(&'static str),
    #[error("Invalid header value for '{0}': {1}")]
    InvalidHeaderValue(&'static str, String),
    #[error("Unknown message type: {0}")]
    UnknownMessageType(String),
    #[error("Internal server error: {0}")]
    InternalServerError(String),
    #[error("Database error: {0}")]
    DatabaseError(#[from] anyhow::Error),
}

impl WebhookError {
    fn status(&self) -> StatusCode {
        use WebhookError::*;
        match self {
            VerificationFailed(_) => StatusCode::FORBIDDEN,
            BadPayload(_) | MissingHeader(_) | InvalidHeaderValue(_, _) | UnknownMessageType(_) => {
                StatusCode::BAD_REQUEST
            }
            DuplicateMessageId(_) => StatusCode::NO_CONTENT,
            InternalServerError(_) | DatabaseError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for WebhookError {
    fn into_response(self) -> Response {
        let status = self.status();

        match status {
            StatusCode::INTERNAL_SERVER_ERROR => error!("{self:?}"),
            _ => warn!("{self:?}"),
        }

        let body = match self {
            WebhookError::InternalServerError(_) | WebhookError::DatabaseError(_) => {
                "Internal Server Error".to_string()
            }
            WebhookError::DuplicateMessageId(_) | WebhookError::VerificationFailed(_) => {
                "".to_string()
            }
            _ => self.to_string(),
        };
        (status, body).into_response()
    }
}

pub type Result<T> = std::result::Result<T, WebhookError>;

fn json<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T> {
    serde_json::from_slice(body)
        .map_err(|e| WebhookError::BadPayload(format!("JSON parse error: {e}")))
}

#[derive(Deserialize, Debug)]
struct ChallengePayload {
    challenge: String,
}

#[derive(Deserialize, Debug)]
pub struct OnlineEvent {
    pub id: String,
    pub broadcaster_user_id: String,
    pub broadcaster_user_name: String,
}

#[derive(Deserialize, Debug)]
pub struct OfflineEvent {
    pub broadcaster_user_id: String,
    pub broadcaster_user_name: String,
}

#[derive(Deserialize, Debug)]
pub struct ChannelUpdateEvent {
    pub broadcaster_user_id: String,
    pub broadcaster_user_name: String,
    pub title: String,
    pub category_name: String,
}

#[derive(Deserialize, Debug)]
pub struct Subscription {
    #[serde(rename = "type")]
    pub kind: String,
}

pub struct Stream {
    pub id: String,
    pub channel_id: String,
    pub user_login: String,
    pub user_name: String,

    pub title: String,
    pub category: String,

    pub events: Vec<db::UpdateEvent>,

    pub started_at: chrono::DateTime<Utc>,
    pub last_updated: chrono::DateTime<Utc>,

    pub message_id: i64,
    pub profile_image_url: String,
}

pub struct TwitchWebhook {
    key: Key<Hmac<Sha256>>,
    port: u16,

    api: Arc<super::twitch::TwitchAPI>,
    pool: sqlx::PgPool,
    recent_messages: ttl_set::TtlSet,
    streams: DashMap<String, Arc<Mutex<Stream>>>,

    tasks: Mutex<tokio::task::JoinSet<()>>,

    channels: DashMap<String, db::Channel>,

    discord_http: Arc<DiscordHttp>,
    discord_channel: ChannelId,
}

impl TwitchWebhook {
    pub(crate) async fn new(
        secret: String,
        port: u16,
        api: Arc<super::twitch::TwitchAPI>,
        pool: sqlx::PgPool,
        channels: Vec<db::Channel>,
        discord_http: Arc<DiscordHttp>,
        discord_channel: ChannelId,
    ) -> Result<Self> {
        let webhook = Self {
            key: Key::<Hmac<Sha256>>::clone_from_slice(secret.as_bytes()),
            port,
            api,
            pool,
            recent_messages: ttl_set::TtlSet::new(),
            streams: DashMap::new(),
            tasks: Mutex::new(tokio::task::JoinSet::new()),
            channels: DashMap::from_iter(channels.into_iter().map(|c| (c.channel_id.clone(), c))),
            discord_http,
            discord_channel,
        };
        webhook.load_streams().await?;
        Ok(webhook)
    }

    pub(crate) async fn track_channel(&self, user_id: &str, channel: db::Channel) -> Result<()> {
        self.channels.insert(channel.channel_id.clone(), channel);
        if let Ok(stream) = self.api.get_stream(user_id, false).await {
            self.handle_stream_online(
                user_id.to_string(),
                Some(stream.clone()),
                None,
                stream.started_at,
            )
            .await?;
        }
        Ok(())
    }

    pub(crate) async fn untrack_channel(&self, channel_id: &str) -> Result<()> {
        self.channels.remove(channel_id);
        if let Some((_, stream)) = self.streams.remove(channel_id) {
            let stream = stream.lock().await;
            self.delete_discord(stream.message_id).await?;
            db::delete_stream(&self.pool, &stream.id).await?;
        }
        Ok(())
    }

    #[instrument(skip(self))]
    async fn load_streams(&self) -> Result<()> {
        let channels = db::list_channels(&self.pool).await?;
        if channels.is_empty() {
            return Ok(());
        }

        let stored: HashMap<String, db::Stream, RandomState> = HashMap::from_iter(
            db::get_streams(&self.pool, None)
                .await?
                .into_iter()
                .map(|s| (s.stream_id.clone(), s)),
        );

        let streams = self
            .api
            .get_streams(
                &channels
                    .iter()
                    .map(|c| c.channel_id.clone())
                    .collect::<Vec<_>>(),
            )
            .await
            .map_err(|e| WebhookError::InternalServerError(format!("Twitch API error: {e:#}")))?;

        let stored_ref = &stored;
        stream::iter(streams)
            .for_each_concurrent(CONCURRENCY_LIMIT, |stream| async move {
                let _ = self
                    .handle_stream_online(
                        stream.user_id.clone(),
                        Some(stream.clone()),
                        stored_ref.get(&stream.id),
                        stream.started_at,
                    )
                    .await
                    .map_err(|e: WebhookError| error!("Error handling stream online: {e:?}"));
            })
            .await;

        Ok(())
    }

    fn header_val<'a>(headers: &'a HeaderMap, header_name: &'static str) -> Result<&'a str> {
        headers
            .get(header_name)
            .ok_or(WebhookError::MissingHeader(header_name))?
            .to_str()
            .map_err(|e| WebhookError::InvalidHeaderValue(header_name, e.to_string()))
    }

    fn signature_headers<'a>(&self, headers: &'a HeaderMap) -> Result<(&'a str, &'a str, &'a str)> {
        let signature = Self::header_val(headers, HEADER_SIGNATURE)?;
        let timestamp = Self::header_val(headers, HEADER_TIMESTAMP)?;
        let message_id = Self::header_val(headers, HEADER_MESSAGE_ID)?;
        Ok((signature, timestamp, message_id))
    }

    #[instrument(skip(self, headers, body))]
    fn verify(&self, headers: &HeaderMap, body: &[u8]) -> Result<DateTime<Utc>> {
        let (raw_signature, timestamp_str, message_id) = self.signature_headers(headers)?;

        if !self
            .recent_messages
            .insert(message_id, tokio::time::Duration::from_secs(10 * 60))
        {
            return Err(WebhookError::DuplicateMessageId(message_id.to_string()));
        }

        let timestamp = DateTime::parse_from_rfc3339(timestamp_str)
            .map_err(|e| {
                WebhookError::InvalidHeaderValue(
                    HEADER_TIMESTAMP,
                    format!("Invalid timestamp: {e}"),
                )
            })?
            .with_timezone(&Utc);

        let now = Utc::now();
        let age = now.signed_duration_since(timestamp);

        if age > chrono::TimeDelta::try_seconds(MAX_TIMESTAMP_AGE_SECONDS as i64).unwrap() {
            return Err(WebhookError::VerificationFailed(
                "Timestamp is too old".to_string(),
            ));
        }

        if age < chrono::TimeDelta::try_seconds(-(MAX_FUTURE_TIMESTAMP_SECONDS as i64)).unwrap() {
            return Err(WebhookError::VerificationFailed(
                "Timestamp is in the future".to_string(),
            ));
        }

        let mut mac: Hmac<Sha256> = hmac::digest::KeyInit::new_from_slice(self.key.as_ref())
            .map_err(|e| WebhookError::InternalServerError(format!("HMAC error: {e}")))?;

        let mut body_with_headers =
            Vec::with_capacity(message_id.len() + timestamp_str.len() + body.len());
        body_with_headers.extend_from_slice(message_id.as_bytes());
        body_with_headers.extend_from_slice(timestamp_str.as_bytes());
        body_with_headers.extend_from_slice(body);

        mac.update(&body_with_headers);

        let signature_to_verify =
            raw_signature
                .strip_prefix(SIGNATURE_PREFIX)
                .ok_or_else(|| {
                    WebhookError::VerificationFailed("Signature missing prefix".to_string())
                })?;

        let received_sig_bytes = hex::decode(signature_to_verify)
            .map_err(|e| WebhookError::VerificationFailed(format!("Invalid hex: {e}")))?;
        mac.verify_slice(&received_sig_bytes)
            .map_err(|_| WebhookError::VerificationFailed("Signature mismatch".into()))?;
        Ok(timestamp)
    }

    fn handle_challenge(&self, body: &Bytes) -> Result<String> {
        let payload = json::<ChallengePayload>(body)?;
        Ok(payload.challenge)
    }

    async fn handle_notification(
        self: &Arc<Self>,
        body: &Bytes,
        timestamp: DateTime<Utc>,
    ) -> Result<()> {
        #[derive(Deserialize, Debug)]
        struct Kind {
            subscription: Subscription,
        }

        #[derive(Deserialize, Debug)]
        struct Notification<T> {
            event: T,
        }

        let Kind { subscription } = json::<Kind>(body)?;
        match subscription.kind.as_str() {
            "stream.online" => {
                let Notification { event } = json::<Notification<OnlineEvent>>(body)?;
                let webhook = Arc::clone(self);
                let user_id = event.broadcaster_user_id.clone();
                {
                    let mut tasks = self.tasks.lock().await;
                    tasks.spawn(async move {
                        if let Err(e) = webhook
                            .handle_stream_online(user_id, None, None, timestamp)
                            .await
                        {
                            error!("Error handling stream online: {e:?}");
                        }
                    });
                }
            }
            "stream.offline" => {
                let Notification { event } = json::<Notification<OfflineEvent>>(body)?;
                self.handle_stream_offline(&event, timestamp).await?;
            }
            "channel.update" => {
                let Notification { event } = json::<Notification<ChannelUpdateEvent>>(body)?;
                self.handle_channel_update(&event, timestamp).await?;
            }
            _ => {
                warn!("Unknown notification type: {}", subscription.kind);
            }
        }
        Ok(())
    }

    pub(crate) async fn handle_stream_online(
        &self,
        user_id: String,
        stream: Option<TwitchStream>,
        preload: Option<&db::Stream>,
        timestamp: DateTime<Utc>,
    ) -> Result<()> {
        let (channel, stream) = match stream {
            Some(stream) => (
                self.api.get_channel(&user_id).await.map_err(|e| {
                    WebhookError::InternalServerError(format!("Twitch API error: {e:#}"))
                })?,
                stream,
            ),
            None => {
                let results = tokio::join!(
                    self.api.get_channel(&user_id),
                    self.api.get_stream(&user_id, true)
                );
                let (channel, stream) = match results {
                    (Err(e), _) => {
                        return Err(WebhookError::InternalServerError(format!(
                            "Twitch API error: {e:#}"
                        )));
                    }
                    (_, Err(e)) => {
                        warn!(
                            "Stream online received for user {} but no stream found: {e:#}",
                            user_id
                        );
                        return Ok(());
                    }
                    (Ok(channel), Ok(stream)) => (channel, stream),
                };

                (channel, stream)
            }
        };

        if self.streams.contains_key(&channel.id) {
            return Ok(());
        }

        {
            let entry = self.channels.entry(channel.id.clone());
            match entry {
                Entry::Occupied(mut occ) => {
                    let stored = occ.get_mut();
                    if channel.login != stored.name || channel.display_name != stored.display_name {
                        stored.name = channel.login.clone();
                        stored.display_name = channel.display_name.clone();
                        db::update_channel(
                            &self.pool,
                            &stored.channel_id,
                            &stored.name,
                            &stored.display_name,
                        )
                        .await?;
                    }
                }
                Entry::Vacant(_) => return Ok(()),
            }
        }

        info!("Stream online received for user: {}", channel.display_name);

        let message_id = match preload.as_ref() {
            Some(stream) => stream.message_id,
            None => self
                .message_discord(
                    CreateMessage::new().embed(
                        CreateEmbed::new()
                            .title(format!(
                                "**{}** is live!",
                                display_name(&channel.display_name, &channel.login)
                            ))
                            .description(&stream.title)
                            .thumbnail(&channel.profile_image_url)
                            .color(colour::Color::from_rgb(145, 70, 255))
                            .url(format!("https://twitch.tv/{}", &channel.login))
                            .field(format!("**»** {}", &stream.game_name), "", true),
                    ),
                )
                .await?
                .id
                .get() as i64,
        };

        self.streams.insert(
            channel.id.clone(),
            Arc::new(Mutex::new(Stream {
                id: stream.id.clone(),
                channel_id: channel.id.clone(),
                user_login: channel.login.clone(),
                user_name: channel.display_name.clone(),
                title: stream.title.clone(),
                category: stream.game_name.clone(),
                started_at: stream.started_at,
                last_updated: stream.started_at,
                events: if let Some(stream) = preload.as_ref() {
                    stream.events.0.clone()
                } else {
                    vec![db::UpdateEvent {
                        title: stream.title.clone(),
                        category: stream.game_name.clone(),
                        timestamp,
                    }]
                },
                message_id,
                profile_image_url: channel.profile_image_url.clone(),
            })),
        );

        if preload.is_none() {
            db::start_stream(
                &self.pool,
                &stream.id,
                &channel.id,
                &stream.title,
                &stream.game_name,
                message_id as u64,
                stream.started_at,
            )
            .await?;
        }

        Ok(())
    }

    pub(crate) async fn handle_stream_offline(
        &self,
        event: &OfflineEvent,
        timestamp: DateTime<Utc>,
    ) -> Result<()> {
        info!(
            "Stream offline received for user: {}",
            event.broadcaster_user_name
        );

        let guard = match self.streams.remove(&event.broadcaster_user_id) {
            Some(guard) => guard,
            None => return Ok(()),
        };

        let stream = guard.1.lock().await;
        if stream.events.is_empty() {
            warn!("{}'s stream has no events", stream.user_name);
            return Ok(());
        }
        let mut events = stream.events.clone();
        events.push(db::UpdateEvent {
            title: stream.title.clone(),
            category: stream.category.clone(),
            timestamp,
        });
        events.sort_by_key(|e| e.timestamp);

        let (title, categories) = tally_categories(&events);

        let mut most: Vec<_> = categories.into_iter().collect();
        most.sort_by_key(|(_, count)| Reverse(*count));
        let category = format!(
            "**»** {}",
            most.into_iter()
                .take(3)
                .map(|e| e.0)
                .collect::<Vec<_>>()
                .join(" ⬩ ")
        );

        let elapsed = human_duration(stream.started_at, timestamp);

        let builder = EditMessage::new().embed(
            CreateEmbed::new()
                .title(format!(
                    "**{}** streamed for {}",
                    display_name(&stream.user_name, &stream.user_login),
                    elapsed
                ))
                .description(title.to_string())
                .thumbnail(stream.profile_image_url.clone())
                .color(colour::Color::from_rgb(128, 128, 128))
                .url(format!("https://twitch.tv/{}", stream.user_login))
                .field(category, "", true),
        );
        self.edit_discord(stream.message_id, builder).await?;

        db::end_stream(&self.pool, &stream.id, title, timestamp).await?;
        Ok(())
    }

    pub(crate) async fn handle_channel_update(
        &self,
        event: &ChannelUpdateEvent,
        timestamp: DateTime<Utc>,
    ) -> Result<()> {
        info!(
            "Channel update received for user: {}",
            event.broadcaster_user_name
        );

        let guard = match self.streams.get(&event.broadcaster_user_id) {
            Some(guard) => guard,
            None => return Ok(()),
        };
        let mut stream = guard.lock().await;
        stream.title = event.title.clone();
        stream.category = event.category_name.clone();
        stream.last_updated = timestamp;

        stream.events.push(db::UpdateEvent {
            title: event.title.clone(),
            category: event.category_name.clone(),
            timestamp,
        });
        db::update_stream(
            &self.pool,
            &stream.id,
            &stream.title,
            stream.events.last().unwrap(),
        )
        .await?;

        let builder = EditMessage::new().embed(
            CreateEmbed::new()
                .title(format!(
                    "**{}** is live!",
                    display_name(&stream.user_name, &stream.user_login)
                ))
                .description(&event.title)
                .thumbnail(&stream.profile_image_url)
                .color(colour::Color::from_rgb(145, 70, 255))
                .url(format!("https://twitch.tv/{}", stream.user_login))
                .field(format!("**»** {}", &event.category_name), "", true),
        );
        self.edit_discord(stream.message_id, builder).await?;

        Ok(())
    }

    pub(crate) async fn message_discord(
        &self,
        message: CreateMessage,
    ) -> Result<serenity::all::Message> {
        self.discord_channel
            .send_message(self.discord_http.clone(), message)
            .await
            .map_err(|e| {
                WebhookError::InternalServerError(format!(
                    "Failed to send message to Discord channel: {e}"
                ))
            })
    }

    pub(crate) async fn edit_discord(
        &self,
        message_id: i64,
        message: EditMessage,
    ) -> Result<Message> {
        self.discord_channel
            .edit_message(
                &self.discord_http,
                MessageId::from(message_id as u64),
                message,
            )
            .await
            .map_err(|e| WebhookError::InternalServerError(format!("Failed to edit message: {e}")))
    }

    pub(crate) async fn delete_discord(&self, message_id: i64) -> Result<()> {
        self.discord_channel
            .delete_message(&self.discord_http, MessageId::from(message_id as u64))
            .await
            .map_err(|e| {
                WebhookError::InternalServerError(format!("Failed to delete message: {e}"))
            })?;
        Ok(())
    }

    pub(crate) async fn serve<F>(
        self: Arc<Self>,
        shutdown: F,
        channels: Vec<db::Channel>,
    ) -> anyhow::Result<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let port = self.port;
        let app = Router::new()
            .route("/webhook/twitch", routing::post(handle_message))
            .with_state(Arc::clone(&self));

        let listener =
            tokio::net::TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, port)).await?;

        self.api
            .sync(
                &channels
                    .iter()
                    .map(|c| c.channel_id.clone())
                    .collect::<Vec<String>>(),
            )
            .await?;

        info!("Stitch webhook server listening: 0.0.0.0:{}", port);
        axum::serve(listener, app.into_make_service())
            .with_graceful_shutdown(shutdown)
            .await?;
        let mut tasks = self.tasks.lock().await;
        while let Some(result) = tasks.join_next().await {
            result.unwrap_or_else(|e| error!("Task failed: {e:?}"));
        }
        Ok(())
    }
}

async fn handle_message(
    State(server): State<Arc<TwitchWebhook>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse> {
    let timestamp = server.verify(&headers, &body)?;

    let msg_type_header = TwitchWebhook::header_val(&headers, HEADER_MESSAGE_TYPE)?;
    match msg_type_header {
        WEBHOOK_VERIFICATION_TYPE => {
            let challenge = server.handle_challenge(&body)?;
            Ok((StatusCode::OK, challenge).into_response())
        }
        NOTIFICATION_TYPE => {
            server.handle_notification(&body, timestamp).await?;
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        unknown_type => Err(WebhookError::UnknownMessageType(unknown_type.to_string())),
    }
}

fn display_name(user_name: &str, user_login: &str) -> String {
    if user_name.to_lowercase() == user_login {
        user_name.to_string()
    } else {
        format!("{user_name} ({user_login})")
    }
}

fn human_duration(start: DateTime<Utc>, end: DateTime<Utc>) -> String {
    let minutes = end.signed_duration_since(start).num_minutes();
    if minutes < 0 {
        return "<in the future>".into();
    }
    let (hours, mins) = (minutes / 60, minutes % 60);
    format!("{hours}h{mins:02}m")
}

fn tally_categories(events: &[db::UpdateEvent]) -> (&str, HashMap<&str, u64>) {
    let mut titles: HashMap<&str, u64> = HashMap::new();
    let mut categories: HashMap<&str, u64> = HashMap::new();

    for window in events.windows(2) {
        let (prev, curr) = (&window[0], &window[1]);
        let elapsed = curr
            .timestamp
            .signed_duration_since(prev.timestamp)
            .num_seconds() as u64;
        *titles.entry(&prev.title).or_insert(0) += elapsed;
        *categories.entry(&prev.category).or_insert(0) += elapsed;
    }

    let title = titles.iter().max_by_key(|(_, count)| *count).unwrap().0;
    (title, categories)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn test_tally_categories() {
        let base_time = Utc.with_ymd_and_hms(2024, 1, 1, 10, 0, 0).unwrap();

        // Test 1: Single title and category
        let events = vec![
            db::UpdateEvent {
                title: "Stream Title".to_string(),
                category: "Gaming".to_string(),
                timestamp: base_time,
            },
            db::UpdateEvent {
                title: "Stream Title".to_string(),
                category: "Gaming".to_string(),
                timestamp: base_time + chrono::Duration::hours(1),
            },
        ];
        let (title, categories) = tally_categories(&events);
        assert_eq!(title, "Stream Title");
        assert_eq!(categories.get("Gaming"), Some(&3600)); // 1 hour

        // Test 2: Multiple titles, single category
        let events = vec![
            db::UpdateEvent {
                title: "Initial Title".to_string(),
                category: "Gaming".to_string(),
                timestamp: base_time,
            },
            db::UpdateEvent {
                title: "Initial Title".to_string(),
                category: "Gaming".to_string(),
                timestamp: base_time + chrono::Duration::hours(1),
            },
            db::UpdateEvent {
                title: "Changed Title".to_string(),
                category: "Gaming".to_string(),
                timestamp: base_time + chrono::Duration::hours(4),
            },
            db::UpdateEvent {
                title: "Final Title".to_string(),
                category: "Gaming".to_string(),
                timestamp: base_time + chrono::Duration::hours(4) + chrono::Duration::minutes(30),
            },
        ];
        let (title, categories) = tally_categories(&events);
        assert_eq!(title, "Initial Title"); // 4 hours vs 30 minutes
        assert_eq!(categories.get("Gaming"), Some(&16200)); // 4.5 hours total

        // Test 3: Multiple categories
        let events = vec![
            db::UpdateEvent {
                title: "Playing Minecraft".to_string(),
                category: "Minecraft".to_string(),
                timestamp: base_time,
            },
            db::UpdateEvent {
                title: "Still Playing".to_string(),
                category: "Minecraft".to_string(),
                timestamp: base_time + chrono::Duration::hours(1) + chrono::Duration::minutes(30),
            },
            db::UpdateEvent {
                title: "Just Chatting".to_string(),
                category: "Just Chatting".to_string(),
                timestamp: base_time + chrono::Duration::hours(4),
            },
            db::UpdateEvent {
                title: "Playing Fortnite".to_string(),
                category: "Fortnite".to_string(),
                timestamp: base_time + chrono::Duration::hours(4) + chrono::Duration::minutes(15),
            },
        ];
        let (title, categories) = tally_categories(&events);
        assert_eq!(title, "Still Playing"); // 2.5 hours
        assert_eq!(categories.get("Minecraft"), Some(&14400)); // 4 hours
        assert_eq!(categories.get("Just Chatting"), Some(&900)); // 15 minutes
        assert_eq!(categories.get("Fortnite"), None); // No duration for last event

        // Test 4: Equal durations
        let events = vec![
            db::UpdateEvent {
                title: "Title A".to_string(),
                category: "Category A".to_string(),
                timestamp: base_time,
            },
            db::UpdateEvent {
                title: "Title B".to_string(),
                category: "Category B".to_string(),
                timestamp: base_time + chrono::Duration::hours(1),
            },
            db::UpdateEvent {
                title: "Title C".to_string(),
                category: "Category C".to_string(),
                timestamp: base_time + chrono::Duration::hours(2),
            },
        ];
        let (title, categories) = tally_categories(&events);
        assert!(title == "Title A" || title == "Title B"); // Both 1 hour
        assert_eq!(categories.get("Category A"), Some(&3600));
        assert_eq!(categories.get("Category B"), Some(&3600));
        assert_eq!(categories.get("Category C"), None);
    }

    #[test]
    #[should_panic]
    fn test_tally_categories_insufficient_events() {
        let base_time = Utc.with_ymd_and_hms(2024, 1, 1, 10, 0, 0).unwrap();
        let events = vec![db::UpdateEvent {
            title: "Only Title".to_string(),
            category: "Only Category".to_string(),
            timestamp: base_time,
        }];
        let _ = tally_categories(&events);
    }
}
