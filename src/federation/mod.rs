//! P2P federation for the furumusic server.
//!
//! When enabled in the admin settings, the server becomes a regular peer of
//! the furumi federation: it publishes its whole visible library (artists,
//! releases, tracks — names and small metadata, never files) into the
//! shared DHT and serves audio, track metadata, cover art and per-artist
//! catalogs to other peers (TUI clients) over the same wire protocols the
//! clients speak among themselves. Serve-only: the server does not search
//! or download from other peers.
//!
//! Settings are the regular admin config entries (`federation_enabled`,
//! `federation_network_id`) and apply on the fly — saving the settings
//! starts, stops or re-joins the node without a server restart.

mod serve;
mod storage;

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use music_dht::{
    ItemKind, ItemSpec, MusicDhtConfig, MusicDhtService, NetworkId, PeerTicket, RendezvousConfig,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::Row as _;

use crate::config::AppConfig;
use storage::PostgresFederationStorage;

pub use serve::{AUDIO_ALPN, CATALOG_ALPN};

/// How often the published library is re-synchronized with the database.
const SYNC_INTERVAL: Duration = Duration::from_secs(60);

struct Running {
    service: Arc<MusicDhtService>,
    network_name: String,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

pub struct Federation {
    /// Transport data directory; server-side DHT state and identity live in PostgreSQL.
    data_dir: PathBuf,
    database_url: std::sync::Mutex<String>,
    pool: tokio::sync::OnceCell<PgPool>,
    running: tokio::sync::Mutex<Option<Running>>,
    last_sync: std::sync::Mutex<Option<String>>,
    last_error: std::sync::Mutex<Option<String>>,
}

fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The process-wide federation handle.
pub fn handle() -> Arc<Federation> {
    static HANDLE: OnceLock<Arc<Federation>> = OnceLock::new();
    Arc::clone(HANDLE.get_or_init(|| {
        Arc::new(Federation {
            data_dir: PathBuf::from(crate::media_paths::resolve_config_path("federation")),
            database_url: std::sync::Mutex::new(String::new()),
            pool: tokio::sync::OnceCell::new(),
            running: tokio::sync::Mutex::new(None),
            last_sync: std::sync::Mutex::new(None),
            last_error: std::sync::Mutex::new(None),
        })
    }))
}

impl Federation {
    fn set_error(&self, message: Option<String>) {
        *lock(&self.last_error) = message;
    }

    async fn pool(&self) -> Result<PgPool> {
        let url = lock(&self.database_url).clone();
        anyhow::ensure!(!url.is_empty(), "database is not configured");
        let pool = self
            .pool
            .get_or_try_init(|| async {
                sqlx::postgres::PgPoolOptions::new()
                    .max_connections(4)
                    .connect(&url)
                    .await
            })
            .await?;
        Ok(pool.clone())
    }

    /// Starts the node at boot when federation was left enabled. The
    /// settings live in the config KV table, so this waits for the database
    /// and resolves the same default → DB → env precedence the config uses.
    pub async fn boot(self: &Arc<Self>, config: &AppConfig) {
        *lock(&self.database_url) = config.database_url.clone();
        if config.database_url.is_empty() {
            return;
        }
        let pool = match self.pool().await {
            Ok(pool) => pool,
            Err(err) => {
                tracing::warn!("federation boot: database unavailable: {err:#}");
                return;
            }
        };
        // `config` carries defaults + env; overlay the DB rows for fields
        // that have no env override (env > DB > default).
        let mut effective = config.clone();
        let rows = sqlx::query(
            "SELECT key, value FROM furumusic__config_entry
             WHERE key IN ('federation_enabled', 'federation_network_id', 'agent_storage_dir')",
        )
        .fetch_all(&pool)
        .await
        .unwrap_or_default();
        for row in rows {
            let key: String = row.get(0);
            let value: String = row.get(1);
            let env_key = format!("FURU_{}", key.to_ascii_uppercase());
            if std::env::var(&env_key).is_ok() {
                continue;
            }
            match key.as_str() {
                "federation_enabled" => {
                    if let Ok(parsed) = value.parse() {
                        effective.federation_enabled = parsed;
                    }
                }
                "federation_network_id" => effective.federation_network_id = value,
                "agent_storage_dir" => {
                    effective.agent_storage_dir =
                        crate::media_paths::resolve_config_path(&value);
                }
                _ => {}
            }
        }
        self.apply(&effective).await;
    }

    /// Applies the effective configuration: starts, stops or re-joins the
    /// node. Called at boot and every time the admin settings are saved.
    pub async fn apply(self: &Arc<Self>, config: &AppConfig) {
        *lock(&self.database_url) = config.database_url.clone();
        let network = config.federation_network_id.trim().to_string();
        if config.federation_enabled && !network.is_empty() {
            if let Err(err) = self.start(network, config.agent_storage_dir.clone()).await {
                tracing::error!("federation start failed: {err:#}");
                self.set_error(Some(format!("start failed: {err}")));
            }
        } else {
            self.stop().await;
        }
    }

    /// Starts the DHT node. Idempotent per network name; a node on another
    /// network is stopped and re-joined.
    async fn start(self: &Arc<Self>, network_name: String, storage_dir: String) -> Result<()> {
        let pool = self.pool().await?;
        let mut guard = self.running.lock().await;
        if let Some(running) = guard.as_ref() {
            if running.network_name == network_name {
                return Ok(());
            }
            stop_running(guard.take()).await;
        }

        let dht_storage = Arc::new(PostgresFederationStorage::new(pool.clone()).await?);
        let secret_key = dht_storage.load_or_create_secret_key().await?;

        let config = MusicDhtConfig::builder()
            .data_dir(&self.data_dir)
            .network_id(NetworkId::from_name(&network_name))
            // Peers of the network find each other knowing only its name.
            .rendezvous(RendezvousConfig::default())
            .stream_protocol(AUDIO_ALPN)
            .stream_protocol(CATALOG_ALPN)
            .build()
            .map_err(|err| anyhow::anyhow!("invalid federation config: {err}"))?;
        let (service, mut events) =
            MusicDhtService::start_with_storage_and_secret_key(config, dht_storage, secret_key)
                .await
                .map_err(|err| anyhow::anyhow!("failed to start the DHT node: {err}"))?;
        let service = Arc::new(service);
        tracing::info!(
            endpoint_id = %service.endpoint_id(),
            network = %network_name,
            "federation started"
        );

        // Drain DHT events into the log; the channel is bounded.
        let event_task = tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                tracing::debug!("federation event: {event:?}");
            }
        });
        // Keep the published library in sync with the database.
        let sync_self = Arc::clone(self);
        let sync_service = Arc::clone(&service);
        let sync_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(SYNC_INTERVAL);
            loop {
                interval.tick().await;
                sync_self.sync_once(&sync_service).await;
            }
        });
        // Serve audio and catalog requests from other peers.
        let audio_acceptor = service
            .stream_acceptor(AUDIO_ALPN)
            .map_err(|err| anyhow::anyhow!("failed to take the audio acceptor: {err}"))?;
        let audio_task = tokio::spawn(serve::serve_audio(
            audio_acceptor,
            pool.clone(),
            storage_dir.clone(),
            service.endpoint_id(),
        ));
        let catalog_acceptor = service
            .stream_acceptor(CATALOG_ALPN)
            .map_err(|err| anyhow::anyhow!("failed to take the catalog acceptor: {err}"))?;
        let catalog_task = tokio::spawn(serve::serve_catalog(
            catalog_acceptor,
            pool,
            storage_dir,
            service.endpoint_id(),
        ));

        *guard = Some(Running {
            service,
            network_name,
            tasks: vec![event_task, sync_task, audio_task, catalog_task],
        });
        self.set_error(None);
        drop(guard);
        // Publish right away instead of waiting for the first timer tick.
        self.spawn_sync_soon().await;
        Ok(())
    }

    async fn stop(&self) {
        let mut guard = self.running.lock().await;
        stop_running(guard.take()).await;
    }

    async fn service(&self) -> Result<Arc<MusicDhtService>> {
        self.running
            .lock()
            .await
            .as_ref()
            .map(|running| Arc::clone(&running.service))
            .context("federation is not running")
    }

    async fn spawn_sync_soon(self: &Arc<Self>) {
        if let Ok(service) = self.service().await {
            let fed = Arc::clone(self);
            tokio::spawn(async move { fed.sync_once(&service).await });
        }
    }

    pub async fn sync_now(self: &Arc<Self>) -> Result<()> {
        let service = self.service().await?;
        self.sync_once(&service).await;
        Ok(())
    }

    async fn sync_once(&self, service: &MusicDhtService) {
        let specs = match self.collect_specs().await {
            Ok(specs) => specs,
            Err(err) => {
                tracing::warn!("federation sync: library read failed: {err:#}");
                self.set_error(Some(format!("library read failed: {err}")));
                return;
            }
        };
        match service.sync_library(specs).await {
            Ok(stats) => {
                *lock(&self.last_sync) = Some(format!(
                    "{} (+{} ~{} −{}, unchanged {}, failed {})",
                    now_iso(),
                    stats.added,
                    stats.updated,
                    stats.removed,
                    stats.unchanged,
                    stats.failed
                ));
                if stats.failed > 0 {
                    self.set_error(Some(format!(
                        "{} item(s) failed to publish in the last sync",
                        stats.failed
                    )));
                } else {
                    self.set_error(None);
                }
            }
            Err(err) => {
                tracing::warn!("federation sync failed: {err}");
                self.set_error(Some(format!("sync failed: {err}")));
            }
        }
    }

    /// Everything the regular player shows, as DHT item specs: non-hidden
    /// artists, releases and tracks (a track also hides with its release).
    async fn collect_specs(&self) -> Result<Vec<ItemSpec>> {
        let pool = self.pool().await?;
        let mut specs = Vec::new();

        let artists =
            sqlx::query("SELECT id, name FROM furumusic__artist WHERE is_hidden = false")
                .fetch_all(&pool)
                .await?;
        for row in &artists {
            let id: i64 = row.get(0);
            specs.push(ItemSpec {
                local_key: format!("artist:{id}"),
                kind: ItemKind::Artist,
                name: row.get(1),
                artist_names: Vec::new(),
                year: None,
                release_type: None,
                duration_seconds: None,
            });
        }

        let release_artists = sqlx::query(
            "SELECT ra.release_id, a.name FROM furumusic__release_artist ra
             JOIN furumusic__artist a ON a.id = ra.artist_id
             ORDER BY ra.release_id, ra.position",
        )
        .fetch_all(&pool)
        .await?;
        let mut artists_of_release: std::collections::HashMap<i64, Vec<String>> =
            Default::default();
        for row in &release_artists {
            artists_of_release
                .entry(row.get(0))
                .or_default()
                .push(row.get(1));
        }
        let releases = sqlx::query(
            "SELECT id, title, year, release_type FROM furumusic__release
             WHERE is_hidden = false",
        )
        .fetch_all(&pool)
        .await?;
        for row in &releases {
            let id: i64 = row.get(0);
            specs.push(ItemSpec {
                local_key: format!("release:{id}"),
                kind: ItemKind::Release,
                name: row.get(1),
                artist_names: artists_of_release.remove(&id).unwrap_or_default(),
                year: row.get(2),
                release_type: row.get(3),
                duration_seconds: None,
            });
        }

        let track_artists = sqlx::query(
            "SELECT ta.track_id, a.name FROM furumusic__track_artist ta
             JOIN furumusic__artist a ON a.id = ta.artist_id
             WHERE ta.role IN ('main', 'featuring')
             ORDER BY ta.track_id, ta.position",
        )
        .fetch_all(&pool)
        .await?;
        let mut artists_of_track: std::collections::HashMap<i64, Vec<String>> = Default::default();
        for row in &track_artists {
            artists_of_track
                .entry(row.get(0))
                .or_default()
                .push(row.get(1));
        }
        let tracks = sqlx::query(
            "SELECT t.id, t.title, COALESCE(t.year, r.year), t.duration_seconds
             FROM furumusic__track t
             JOIN furumusic__release r ON r.id = t.release_id
             WHERE t.is_hidden = false AND r.is_hidden = false",
        )
        .fetch_all(&pool)
        .await?;
        for row in &tracks {
            let id: i64 = row.get(0);
            let duration: f64 = row.get(3);
            specs.push(ItemSpec {
                local_key: format!("track:{id}"),
                kind: ItemKind::Track,
                name: row.get(1),
                artist_names: artists_of_track.remove(&id).unwrap_or_default(),
                year: row.get(2),
                release_type: None,
                duration_seconds: (duration > 0.0).then_some(duration),
            });
        }

        Ok(specs)
    }

    /// Live status for the admin page.
    pub async fn status(&self) -> Value {
        let guard = self.running.lock().await;
        let node = match guard.as_ref() {
            Some(running) => {
                let service = &running.service;
                let published = service
                    .list_local_items()
                    .await
                    .map(|items| items.len())
                    .unwrap_or(0);
                let peers: Vec<String> = service
                    .connected_peers()
                    .iter()
                    .map(|p| p.to_string())
                    .collect();
                json!({
                    "running": true,
                    "network": running.network_name,
                    "endpoint_id": service.endpoint_id().to_string(),
                    "connected_peers": peers,
                    "known_contacts": service.known_peers().len(),
                    "published_items": published,
                })
            }
            None => json!({ "running": false }),
        };
        json!({
            "node": node,
            "last_sync": lock(&self.last_sync).clone(),
            "last_error": lock(&self.last_error).clone(),
        })
    }

    pub async fn ticket(&self) -> Result<String> {
        let service = self.service().await?;
        let ticket = service
            .ticket()
            .await
            .map_err(|err| anyhow::anyhow!("cannot create a ticket: {err}"))?;
        Ok(ticket.to_string())
    }

    pub async fn connect(&self, ticket: &str) -> Result<String> {
        let service = self.service().await?;
        let ticket: PeerTicket = ticket
            .trim()
            .parse()
            .map_err(|err| anyhow::anyhow!("malformed ticket: {err}"))?;
        let peer = service
            .connect(ticket)
            .await
            .map_err(|err| anyhow::anyhow!("connect failed: {err}"))?;
        Ok(peer.to_string())
    }
}

async fn stop_running(running: Option<Running>) {
    let Some(running) = running else { return };
    for task in &running.tasks {
        task.abort();
    }
    if let Err(err) = running.service.shutdown().await {
        tracing::warn!("federation node shutdown reported an error: {err}");
    }
    tracing::info!("federation stopped");
}
