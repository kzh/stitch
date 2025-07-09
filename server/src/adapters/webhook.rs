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
use dashmap::DashMap;
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
use std::collections::hash_map::RandomState;
use std::{collections::HashMap, sync::Arc};
use tokio::{sync::Mutex, try_join};
use tracing::{error, info, instrument, warn};

const SIGNATURE_PREFIX: &str = "sha256=";
const WEBHOOK_VERIFICATION_TYPE: &str = "webhook_callback_verification";
const NOTIFICATION_TYPE: &str = "notification";
const MAX_TIMESTAMP_AGE_SECONDS: u64 = 600;
const MAX_FUTURE_TIMESTAMP_SECONDS: u64 = 60;

const HEADER_SIGNATURE: &str = "Twitch-Eventsub-Message-Signature";
const HEADER_TIMESTAMP: &str = "Twitch-Eventsub-Message-Timestamp";
const HEADER_MESSAGE_ID: &str = "Twitch-Eventsub-Message-Id";
const HEADER_MESSAGE_TYPE: &str = "Twitch-Eventsub-Message-Type";

const CONCURRENCY_LIMIT: usize = 20;

#[derive(thiserror::Error, Debug)]
pub enum WebhookError {
    #[error("Verification failed: {0}")]
    VerificationFailed(String),
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
            VerificationFailed(_) => StatusCode::UNAUTHORIZED,
            BadPayload(_) | MissingHeader(_) | InvalidHeaderValue(_, _) | UnknownMessageType(_) => {
                StatusCode::BAD_REQUEST
            }
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
            _ => self.to_string(),
        };
        (status, body).into_response()
    }
}

pub type Result<T> = std::result::Result<T, WebhookError>;

fn json<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T> {
    serde_json::from_slice(body)
        .map_err(|e| WebhookError::BadPayload(format!("JSON parse error: {}", e)))
}

#[derive(Deserialize, Debug)]
struct ChallengePayload {
    challenge: String,
}

#[derive(Deserialize, Debug)]
pub struct OnlineEvent {
    pub broadcaster_user_id: String,
    pub broadcaster_user_name: String,
    pub category_id: Option<String>,
    pub category_name: Option<String>,
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

    discord_http: Arc<DiscordHttp>,
    discord_channel: ChannelId,
}

impl TwitchWebhook {
    pub async fn new(
        secret: String,
        port: u16,
        api: Arc<super::twitch::TwitchAPI>,
        pool: sqlx::PgPool,
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
            discord_http,
            discord_channel,
        };
        webhook.load_streams().await?;
        Ok(webhook)
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
            .map_err(|e| WebhookError::InternalServerError(format!("Twitch API error: {:#}", e)))?;

        let stored_ref = &stored;
        stream::iter(streams)
            .for_each_concurrent(CONCURRENCY_LIMIT, |stream| async move {
                let _ = self
                    .handle_stream_online(
                        &stream.user_id,
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

    #[instrument(skip(self, headers, body), err(Debug))]
    fn verify(&self, headers: &HeaderMap, body: &[u8]) -> Result<DateTime<Utc>> {
        let (raw_signature, timestamp_str, message_id) = self.signature_headers(headers)?;

        if self.recent_messages.contains(message_id) {
            return Err(WebhookError::VerificationFailed(format!(
                "Duplicate message ID: {}",
                message_id
            )));
        }
        self.recent_messages.insert(
            message_id.to_string(),
            tokio::time::Duration::from_secs(10 * 60),
        );

        let timestamp = DateTime::parse_from_rfc3339(timestamp_str)
            .map_err(|e| {
                WebhookError::InvalidHeaderValue(
                    HEADER_TIMESTAMP,
                    format!("Invalid timestamp: {}", e),
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
            .map_err(|e| WebhookError::InternalServerError(format!("HMAC error: {}", e)))?;

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
            .map_err(|e| WebhookError::VerificationFailed(format!("Invalid hex: {}", e)))?;
        mac.verify_slice(&received_sig_bytes)
            .map_err(|_| WebhookError::VerificationFailed("Signature mismatch".into()))?;
        Ok(timestamp)
    }

    fn handle_challenge(&self, body: &Bytes) -> Result<String> {
        let payload = json::<ChallengePayload>(body)?;
        Ok(payload.challenge)
    }

    async fn handle_notification(&self, body: &Bytes, timestamp: DateTime<Utc>) -> Result<()> {
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
                self.handle_stream_online(&event.broadcaster_user_id, None, None, timestamp)
                    .await?;
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
        user_id: &str,
        stream: Option<TwitchStream>,
        preload: Option<&db::Stream>,
        timestamp: DateTime<Utc>,
    ) -> Result<()> {
        let (channel, stream) = if let Some(stream) = stream {
            let channel = self.api.get_channel(user_id).await.map_err(|e| {
                WebhookError::InternalServerError(format!("Twitch API error: {:#}", e))
            })?;
            (channel, stream)
        } else {
            let (channel, stream) =
                try_join!(self.api.get_channel(user_id), self.api.get_stream(user_id)).map_err(
                    |e| WebhookError::InternalServerError(format!("Twitch API error: {:#}", e)),
                )?;
            (channel, stream)
        };

        if self.streams.contains_key(&stream.id) {
            return Ok(());
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
                        timestamp: timestamp,
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
        let mut events = stream.events.clone();
        events.push(db::UpdateEvent {
            title: stream.title.clone(),
            category: stream.category.clone(),
            timestamp,
        });
        events.sort_by_key(|e| e.timestamp);

        let mut last_event = stream.events.first().unwrap().timestamp;
        let mut titles = HashMap::new();
        let mut categories = HashMap::new();
        for event in events {
            let elapsed = event
                .timestamp
                .signed_duration_since(last_event)
                .num_seconds();
            last_event = event.timestamp;
            titles
                .entry(event.title.clone())
                .and_modify(|count| *count += elapsed)
                .or_insert(elapsed);
            categories
                .entry(event.category.clone())
                .and_modify(|count| *count += elapsed)
                .or_insert(elapsed);
        }

        let title = titles.iter().max_by_key(|(_, count)| *count).unwrap().0;
        let category = categories.iter().max_by_key(|(_, count)| *count).unwrap().0;

        let elapsed = human_duration(stream.started_at, timestamp);

        let builder = EditMessage::new().embed(
            CreateEmbed::new()
                .title(format!(
                    "**{}** streamed for {}",
                    display_name(&stream.user_name, &stream.user_login),
                    elapsed
                ))
                .description(title)
                .thumbnail(stream.profile_image_url.clone())
                .color(colour::Color::from_rgb(128, 128, 128))
                .url(format!("https://twitch.tv/{}", stream.user_login))
                .field(format!("**»** {}", &category), "", true),
        );
        self.edit_discord(stream.message_id, builder).await?;

        db::end_stream(&self.pool, &stream.id, &title, timestamp).await?;
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
            &stream.events.last().unwrap(),
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
                    "Failed to send message to Discord channel: {}",
                    e
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
            .map_err(|e| {
                WebhookError::InternalServerError(format!("Failed to edit message: {}", e))
            })
    }

    pub(crate) async fn serve(self) -> anyhow::Result<()> {
        let port = self.port;
        let app = Router::new()
            .route("/webhook/twitch", routing::post(handle_message))
            .with_state(Arc::new(self));

        let listener =
            tokio::net::TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, port)).await?;

        info!("Stitch webhook server listening: 0.0.0.0:{}", port);
        axum::serve(listener, app.into_make_service()).await?;
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
        format!("{} ({})", user_name, user_login)
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
