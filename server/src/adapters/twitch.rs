use std::collections::{HashMap, HashSet};

use anyhow::Context;
use chrono::{DateTime, Utc};
use futures::future::try_join_all;
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use tracing::{info, instrument};

const TWITCH_OAUTH_URL: &str = "https://id.twitch.tv/oauth2/token";
const TWITCH_HELIX_USERS_URL: &str = "https://api.twitch.tv/helix/users";
const TWITCH_HELIX_STREAMS_URL: &str = "https://api.twitch.tv/helix/streams";
const TWITCH_EVENTSUB_URL: &str = "https://api.twitch.tv/helix/eventsub/subscriptions";

const STREAM_FETCH_RETRY_DELAY_SECS: &[u64; 5] = &[15, 30, 60, 120, 300];

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_owned()
    } else {
        format!("{}…", &s[..max])
    }
}

#[derive(Deserialize)]
pub struct StreamsResponse {
    pub data: Vec<TwitchStream>,
}

#[derive(Deserialize, Clone)]
pub struct TwitchStream {
    pub id: String,
    pub user_id: String,
    pub user_login: String,
    pub user_name: String,
    pub game_id: String,
    pub game_name: String,
    pub title: String,
    pub started_at: DateTime<Utc>,
}

#[derive(Deserialize)]
pub struct ChannelsResponse {
    data: Vec<TwitchChannel>,
}

#[derive(Deserialize)]
pub struct TwitchChannel {
    pub id: String,
    pub login: String,
    pub display_name: String,
    pub description: String,
    pub profile_image_url: String,
}

#[derive(Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
}

#[derive(Deserialize, Debug)]
pub struct SubscriptionCondition {
    pub broadcaster_user_id: String,
}

#[derive(Deserialize, Debug)]
pub struct Subscription {
    pub id: String,
    pub status: String,
    pub condition: SubscriptionCondition,

    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Deserialize, Debug)]
pub struct Pagination {
    pub cursor: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct SubscriptionResponse {
    pub data: Vec<Subscription>,
    pub pagination: Pagination,
}

#[instrument(skip_all)]
async fn get_access_token(client_id: &str, client_secret: &str) -> anyhow::Result<String> {
    let resp = Client::new()
        .post(TWITCH_OAUTH_URL)
        .query(&[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("grant_type", "client_credentials"),
        ])
        .send()
        .await
        .context("Failed to request OAuth token")?
        .error_for_status()
        .context("Twitch returned non‑2xx response for OAuth token")?
        .json::<TokenResponse>()
        .await?;

    Ok(resp.access_token)
}

pub struct TwitchAPI {
    client_id: String,
    access_token: String,
    webhook_url: String,
    webhook_secret: String,
    http_client: Client,
}

impl TwitchAPI {
    pub async fn new(
        client_id: String,
        client_secret: String,
        webhook_url: String,
        webhook_secret: String,
    ) -> anyhow::Result<Self> {
        let access_token = get_access_token(&client_id, &client_secret).await?;
        let http_client = Client::new();

        Ok(Self {
            client_id,
            access_token,
            webhook_url,
            webhook_secret,
            http_client,
        })
    }

    fn authenticated_request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        self.http_client
            .request(method, url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Client-Id", &self.client_id)
    }

    async fn send_json<T: serde::de::DeserializeOwned>(
        &self,
        rb: reqwest::RequestBuilder,
        ctx: &'static str,
    ) -> anyhow::Result<T> {
        use anyhow::Context as _;
        let resp = rb.send().await.context(ctx)?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("(failed to read body: {e})"));
        if !status.is_success() {
            anyhow::bail!("{ctx}: Twitch {status}: {}", truncate(&body, 256));
        }
        serde_json::from_str::<T>(&body).context(ctx)
    }

    pub async fn sync(&self, channels: &[String]) -> anyhow::Result<()> {
        let (subs, stale): (Vec<Subscription>, Vec<Subscription>) = self
            .get_subscriptions(None)
            .await?
            .into_iter()
            .partition(|sub| sub.status == "enabled");
        futures::future::join_all(stale.iter().map(|sub| self.unsubscribe(&sub.id))).await;

        let have: HashMap<(&str, &str), &str> = subs
            .iter()
            .map(|e| {
                (
                    (e.condition.broadcaster_user_id.as_str(), e.kind.as_str()),
                    e.id.as_str(),
                )
            })
            .collect();
        let want: HashSet<(&str, &str)> = channels
            .iter()
            .flat_map(|c| {
                [
                    (c.as_str(), "stream.online"),
                    (c.as_str(), "channel.update"),
                    (c.as_str(), "stream.offline"),
                ]
            })
            .collect();

        let add = want
            .iter()
            .filter(|e| !have.contains_key(e))
            .collect::<Vec<_>>();
        futures::future::join_all(
            add.iter()
                .map(|(channel, event)| self.subscribe(event, channel)),
        )
        .await;

        let remove = have
            .iter()
            .filter(|e| !want.contains(e.0))
            .map(|e| e.1)
            .collect::<Vec<_>>();
        futures::future::join_all(remove.iter().map(|id| self.unsubscribe(id))).await;

        info!(
            "Twitch webhooks synchronized for {} channels: {} added, {} removed, {} kept",
            channels.len(),
            add.len(),
            remove.len(),
            have.len() - remove.len()
        );
        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn get_channel(&self, user_id: &str) -> anyhow::Result<TwitchChannel> {
        let resp: ChannelsResponse = self
            .send_json(
                self.authenticated_request(reqwest::Method::GET, TWITCH_HELIX_USERS_URL)
                    .query(&[("id", user_id)]),
                "fetch channel by user_id",
            )
            .await?;

        resp.data
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No user found for id: {}", user_id))
    }

    #[instrument(skip(self))]
    pub async fn get_stream(&self, user_id: &str, retry: bool) -> anyhow::Result<TwitchStream> {
        let attempts = if retry {
            STREAM_FETCH_RETRY_DELAY_SECS.len()
        } else {
            0
        };

        let mut last_error: Option<anyhow::Error> = None;
        for attempt in 0..=attempts {
            match self
                .send_json::<StreamsResponse>(
                    self.authenticated_request(reqwest::Method::GET, TWITCH_HELIX_STREAMS_URL)
                        .query(&[("user_id", user_id)]),
                    "fetch stream by user_id",
                )
                .await
            {
                Ok(resp) => {
                    if let Some(stream) = resp.data.into_iter().next() {
                        return Ok(stream);
                    } else if attempt == attempts {
                        last_error =
                            Some(anyhow::anyhow!("No stream found for user_id: {}", user_id));
                    }
                }
                Err(e) => {
                    tracing::error!("Error fetching stream: {e:?}");
                    if attempt == attempts {
                        last_error = Some(e);
                    }
                }
            }

            if attempt < attempts {
                let sleep_duration = STREAM_FETCH_RETRY_DELAY_SECS[attempt];
                tokio::time::sleep(tokio::time::Duration::from_secs(sleep_duration)).await;
            }
        }

        anyhow::bail!(
            "Failed to fetch stream after {} retries: {last_error:?}",
            attempts
        );
    }

    #[instrument(skip(self))]
    pub async fn get_streams(&self, user_ids: &[String]) -> anyhow::Result<Vec<TwitchStream>> {
        let mut streams: Vec<TwitchStream> = Vec::new();

        for chunk in user_ids.chunks(100) {
            let resp: StreamsResponse = self
                .send_json(
                    self.authenticated_request(reqwest::Method::GET, TWITCH_HELIX_STREAMS_URL)
                        .query(&chunk.iter().map(|id| ("user_id", id)).collect::<Vec<_>>()),
                    "fetch streams by user_ids",
                )
                .await?;
            streams.extend(resp.data);
        }

        Ok(streams)
    }

    #[instrument(skip(self))]
    pub async fn get_channel_by_name(&self, username: &str) -> anyhow::Result<TwitchChannel> {
        let resp: ChannelsResponse = self
            .send_json(
                self.authenticated_request(reqwest::Method::GET, TWITCH_HELIX_USERS_URL)
                    .query(&[("login", username)]),
                "fetch channel by username",
            )
            .await?;

        resp.data
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No user found for username: {}", username))
    }

    #[instrument(skip(self))]
    pub async fn subscribe(&self, event: &str, user_id: &str) -> anyhow::Result<Value> {
        let payload = serde_json::json!({
            "type": event,
            "version": "1",
            "condition": { "broadcaster_user_id": user_id },
            "transport": {
                "method":   "webhook",
                "callback": format!("https://{}/webhook/twitch", &self.webhook_url),
                "secret":   &self.webhook_secret,
            },
        });

        let resp: Value = self
            .send_json(
                self.authenticated_request(reqwest::Method::POST, TWITCH_EVENTSUB_URL)
                    .header("Content-Type", "application/json")
                    .json(&payload),
                "create subscription",
            )
            .await?;
        Ok(resp)
    }

    #[instrument(skip(self))]
    pub async fn subscribe_channel(&self, user_id: &str) -> anyhow::Result<()> {
        futures::try_join!(
            self.subscribe("stream.online", user_id),
            self.subscribe("channel.update", user_id),
            self.subscribe("stream.offline", user_id),
        )?;

        info!("Subscription created for user_id: {}", user_id);
        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn unsubscribe(&self, subscription_id: &str) -> anyhow::Result<()> {
        self.authenticated_request(reqwest::Method::DELETE, TWITCH_EVENTSUB_URL)
            .query(&[("id", subscription_id)])
            .send()
            .await
            .context("Failed to send unsubscribe request")?
            .error_for_status()
            .context("Twitch returned non‑2xx response while unsubscribing")?;
        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn unsubscribe_channel(&self, user_id: &str) -> anyhow::Result<()> {
        let subscriptions = self.get_subscriptions(Some(user_id)).await?;
        if subscriptions.is_empty() {
            return Ok(());
        }

        let responses = try_join_all(
            subscriptions
                .iter()
                .map(|subscription| self.unsubscribe(&subscription.id)),
        )
        .await?;

        info!(?responses, "Unsubscribed user_id: {}", user_id);
        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn get_subscriptions(
        &self,
        channel: Option<&str>,
    ) -> anyhow::Result<Vec<Subscription>> {
        let mut subscriptions = Vec::new();
        let mut next: Option<String> = None;
        loop {
            let mut request = self.authenticated_request(reqwest::Method::GET, TWITCH_EVENTSUB_URL);
            if let Some(channel) = channel {
                request = request.query(&[("user_id", channel)]);
            }
            if let Some(ref cursor) = next {
                request = request.query(&[("after", cursor.as_str())]);
            }

            let resp: SubscriptionResponse = self.send_json(request, "fetch subscriptions").await?;
            subscriptions.extend(resp.data);
            if let Some(cursor) = resp.pagination.cursor {
                next = Some(cursor);
            } else {
                break;
            }
        }
        Ok(subscriptions)
    }
}
