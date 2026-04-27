use anyhow::Result;
use nostr_sdk::{Client, Filter};

const SUBSCRIBE_TIMEOUT_SECS: u64 = 3;
const SEND_EVENT_TIMEOUT_SECS: u64 = 2;

use crate::config::Config;

pub(crate) async fn connect_client(config: &Config) -> Result<Client> {
    let client = Client::default();
    let relays = config.resolved_relays();
    for relay in &relays {
        client.add_relay(relay).await?;
    }
    client.connect().await;
    Ok(client)
}

async fn send_event_once(client: &Client, event: nostr::Event) -> Result<()> {
    let relays = client.relays().await;
    if relays.is_empty() {
        match tokio::time::timeout(
            std::time::Duration::from_secs(SEND_EVENT_TIMEOUT_SECS),
            client.send_event(&event),
        )
        .await
        {
            Ok(Ok(_)) => return Ok(()),
            Ok(Err(err)) => return Err(err.into()),
            Err(_) => anyhow::bail!("send_event timed out"),
        }
    }

    let mut relay_urls: Vec<String> = relays.keys().map(|url| url.to_string()).collect();
    relay_urls.sort();

    let mut last_error: Option<anyhow::Error> = None;
    let mut any_success = false;

    for relay in relay_urls {
        match tokio::time::timeout(
            std::time::Duration::from_secs(SEND_EVENT_TIMEOUT_SECS),
            client.send_event_to([relay.as_str()], &event),
        )
        .await
        {
            Ok(Ok(_)) => {
                any_success = true;
            }
            Ok(Err(err)) => {
                last_error = Some(err.into());
            }
            Err(_) => {
                last_error = Some(anyhow::anyhow!("send_event timed out for relay {}", relay));
            }
        }
    }

    if any_success {
        Ok(())
    } else if let Some(err) = last_error {
        Err(err)
    } else {
        Err(anyhow::anyhow!("failed to publish event to any relay"))
    }
}

pub(crate) async fn send_event_or_ignore(client: &Client, event: nostr::Event) -> Result<()> {
    match send_event_once(client, event).await {
        Ok(()) => Ok(()),
        Err(_) if should_ignore_publish_errors() => Ok(()),
        Err(err) => Err(err),
    }
}

pub(crate) async fn send_event_with_retry(
    client: &Client,
    event: nostr::Event,
    attempts: usize,
    retry_ms: u64,
) -> Result<()> {
    let attempts = attempts.max(1);
    let mut last_error: Option<anyhow::Error> = None;

    for attempt in 0..attempts {
        match send_event_once(client, event.clone()).await {
            Ok(()) => return Ok(()),
            Err(err) => {
                last_error = Some(err);
            }
        }

        if attempt + 1 < attempts {
            tokio::time::sleep(std::time::Duration::from_millis(retry_ms)).await;
        }
    }

    if should_ignore_publish_errors() {
        Ok(())
    } else if let Some(err) = last_error {
        Err(err)
    } else {
        Err(anyhow::anyhow!("failed to publish event to any relay"))
    }
}

pub(crate) async fn send_events_with_retry(
    client: &Client,
    events: &[nostr::Event],
    attempts: usize,
    retry_ms: u64,
) -> Result<bool> {
    if events.is_empty() {
        return Ok(false);
    }

    let attempts = attempts.max(1);
    let mut last_error: Option<anyhow::Error> = None;

    for attempt in 0..attempts {
        let mut all_ok = true;
        for event in events {
            if let Err(err) = send_event_once(client, event.clone()).await {
                last_error = Some(err);
                all_ok = false;
                break;
            }
        }

        if all_ok {
            return Ok(true);
        }

        if attempt + 1 < attempts {
            tokio::time::sleep(std::time::Duration::from_millis(retry_ms)).await;
        }
    }

    if should_ignore_publish_errors() {
        Ok(false)
    } else if let Some(err) = last_error {
        Err(err)
    } else {
        Err(anyhow::anyhow!(
            "failed to publish event batch to any relay"
        ))
    }
}

pub(crate) async fn subscribe_filters_best_effort(
    client: &Client,
    relays: &[String],
    filters: Vec<Filter>,
) -> Result<()> {
    if relays.is_empty() {
        for filter in filters {
            client.subscribe(filter, None).await?;
        }
        return Ok(());
    }

    let mut last_error: Option<anyhow::Error> = None;
    let mut any_success = false;

    for relay in relays {
        match tokio::time::timeout(
            std::time::Duration::from_secs(SUBSCRIBE_TIMEOUT_SECS),
            subscribe_filters_to_relay(client, relay, filters.clone()),
        )
        .await
        {
            Ok(Ok(_)) => {
                any_success = true;
            }
            Ok(Err(err)) => {
                last_error = Some(err.into());
            }
            Err(_) => {
                last_error = Some(anyhow::anyhow!("subscribe timed out for relay {}", relay));
            }
        }
    }

    if any_success {
        return Ok(());
    }

    if let Some(err) = last_error {
        Err(err)
    } else {
        Err(anyhow::anyhow!("failed to subscribe to any relay"))
    }
}

async fn subscribe_filters_to_relay(
    client: &Client,
    relay: &str,
    filters: Vec<Filter>,
) -> Result<()> {
    for filter in filters {
        client.subscribe_to([relay], filter, None).await?;
    }
    Ok(())
}

pub(crate) async fn fetch_events_best_effort(
    client: &Client,
    relays: &[String],
    filter: Filter,
    timeout: std::time::Duration,
) -> Result<Vec<nostr::Event>> {
    if relays.is_empty() {
        let events = client.fetch_events(filter, timeout).await?;
        return Ok(events.iter().cloned().collect());
    }

    let mut last_error: Option<anyhow::Error> = None;
    let mut any_success = false;
    let mut seen_event_ids = std::collections::HashSet::new();
    let mut collected = Vec::new();

    for relay in relays {
        match tokio::time::timeout(
            timeout + std::time::Duration::from_millis(250),
            client.fetch_events_from([relay.as_str()], filter.clone(), timeout),
        )
        .await
        {
            Ok(Ok(events)) => {
                any_success = true;
                for event in events.iter() {
                    let event_id = event.id;
                    if seen_event_ids.insert(event_id) {
                        collected.push(event.clone());
                    }
                }
            }
            Ok(Err(err)) => {
                last_error = Some(err.into());
            }
            Err(_) => {
                last_error = Some(anyhow::anyhow!(
                    "fetch_events timed out for relay {}",
                    relay
                ));
            }
        }
    }

    if any_success {
        return Ok(collected);
    }

    if let Some(err) = last_error {
        Err(err)
    } else {
        Err(anyhow::anyhow!("failed to fetch events from any relay"))
    }
}

pub(crate) fn should_ignore_publish_errors() -> bool {
    for key in ["NDR_IGNORE_PUBLISH_ERRORS", "NOSTR_IGNORE_PUBLISH_ERRORS"] {
        if let Ok(val) = std::env::var(key) {
            let val = val.trim().to_lowercase();
            return matches!(val.as_str(), "1" | "true" | "yes" | "on");
        }
    }
    false
}
