use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use base64::Engine;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, ManagedTorrent, Session, SessionOptions,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, OnceCell};
use uuid::Uuid;

use crate::scheduler::SchedulerHandle;

const METADATA_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug, Clone, Serialize)]
pub struct TorrentFileDto {
    pub index: usize,
    pub name: String,
    pub components: Vec<String>,
    pub length: u64,
    pub selected: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TorrentPreviewDto {
    pub id: String,
    pub name: String,
    pub info_hash: String,
    pub total_size: u64,
    pub files: Vec<TorrentFileDto>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TorrentJobDto {
    pub id: String,
    pub name: String,
    pub info_hash: String,
    pub status: String,
    pub total_size: u64,
    pub selected_size: u64,
    pub downloaded_bytes: u64,
    pub progress_percent: f64,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TorrentPreviewKind {
    Magnet,
    TorrentFile,
}

#[derive(Debug, Deserialize)]
pub struct TorrentPreviewRequest {
    pub kind: TorrentPreviewKind,
    pub magnet: Option<String>,
    pub torrent_base64: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TorrentStartRequest {
    pub selected_files: Vec<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TorrentJobStatus {
    Preview,
    Downloading,
    Moving,
    Complete,
    Failed,
}

impl TorrentJobStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Preview => "preview",
            Self::Downloading => "downloading",
            Self::Moving => "moving",
            Self::Complete => "complete",
            Self::Failed => "failed",
        }
    }
}

struct TorrentJob {
    id: String,
    name: String,
    info_hash: String,
    torrent_bytes: Vec<u8>,
    files: Vec<TorrentFileDto>,
    status: TorrentJobStatus,
    output_dir: PathBuf,
    selected_files: Vec<usize>,
    handle: Option<Arc<ManagedTorrent>>,
    error: Option<String>,
}

impl TorrentJob {
    fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.length).sum()
    }

    fn selected_size(&self) -> u64 {
        if self.selected_files.is_empty() {
            return 0;
        }
        self.files
            .iter()
            .filter(|f| self.selected_files.contains(&f.index))
            .map(|f| f.length)
            .sum()
    }

    fn dto(&self) -> TorrentJobDto {
        let stats = self.handle.as_ref().map(|h| h.stats());
        let downloaded_bytes = stats.as_ref().map(|s| s.progress_bytes).unwrap_or(0);
        let total_bytes = stats
            .as_ref()
            .map(|s| s.total_bytes)
            .filter(|v| *v > 0)
            .unwrap_or_else(|| self.selected_size());
        let progress_percent = if total_bytes == 0 {
            0.0
        } else {
            downloaded_bytes as f64 / total_bytes as f64 * 100.0
        };

        Self::dto_from_parts(
            &self.id,
            &self.name,
            &self.info_hash,
            self.status,
            self.total_size(),
            self.selected_size(),
            downloaded_bytes,
            progress_percent,
            self.error.clone(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn dto_from_parts(
        id: &str,
        name: &str,
        info_hash: &str,
        status: TorrentJobStatus,
        total_size: u64,
        selected_size: u64,
        downloaded_bytes: u64,
        progress_percent: f64,
        error: Option<String>,
    ) -> TorrentJobDto {
        TorrentJobDto {
            id: id.to_string(),
            name: name.to_string(),
            info_hash: info_hash.to_string(),
            status: status.as_str().to_string(),
            total_size,
            selected_size,
            downloaded_bytes,
            progress_percent: progress_percent.clamp(0.0, 100.0),
            error,
        }
    }
}

pub struct TorrentService {
    temp_root: PathBuf,
    session: OnceCell<Arc<Session>>,
    jobs: Mutex<HashMap<String, TorrentJob>>,
    scheduler_handle: Arc<OnceCell<Arc<SchedulerHandle>>>,
}

impl TorrentService {
    pub fn new(scheduler_handle: Arc<OnceCell<Arc<SchedulerHandle>>>) -> Self {
        Self {
            temp_root: std::env::temp_dir().join("furumusic").join("torrents"),
            session: OnceCell::new(),
            jobs: Mutex::new(HashMap::new()),
            scheduler_handle,
        }
    }

    async fn session(&self) -> anyhow::Result<Arc<Session>> {
        let temp_root = self.temp_root.clone();
        self.session
            .get_or_try_init(|| async move {
                tokio::fs::create_dir_all(&temp_root).await?;
                Session::new_with_opts(
                    temp_root,
                    SessionOptions {
                        disable_upload: true,
                        enable_upnp_port_forwarding: false,
                        ..Default::default()
                    },
                )
                .await
            })
            .await
            .cloned()
    }

    pub async fn preview(
        &self,
        request: TorrentPreviewRequest,
    ) -> anyhow::Result<TorrentPreviewDto> {
        let session = self.session().await?;
        let id = Uuid::new_v4().to_string();
        let output_dir = self.temp_root.join(&id).join("download");
        tokio::fs::create_dir_all(&output_dir).await?;

        let add = match request.kind {
            TorrentPreviewKind::Magnet => {
                let magnet = request
                    .magnet
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .context("magnet link is empty")?;
                AddTorrent::from_url(magnet.to_string())
            }
            TorrentPreviewKind::TorrentFile => {
                let encoded = request
                    .torrent_base64
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .context("torrent file is empty")?;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .context("invalid torrent file encoding")?;
                AddTorrent::from_bytes(bytes)
            }
        };

        let response = tokio::time::timeout(
            METADATA_TIMEOUT,
            session.add_torrent(
                add,
                Some(AddTorrentOptions {
                    list_only: true,
                    output_folder: Some(output_dir.to_string_lossy().to_string()),
                    ..Default::default()
                }),
            ),
        )
        .await
        .context("timed out while resolving torrent metadata")??;

        let AddTorrentResponse::ListOnly(list) = response else {
            bail!("torrent was unexpectedly added instead of previewed");
        };

        let name = list
            .info
            .name
            .as_ref()
            .map(|b| String::from_utf8_lossy(b.as_ref()).to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| list.info_hash.as_string());

        let mut files = Vec::new();
        for (index, details) in list.info.iter_file_details()?.enumerate() {
            let name = details
                .filename
                .to_string()
                .unwrap_or_else(|_| "<invalid filename>".to_string());
            let selected = is_audio_path(&name);
            files.push(TorrentFileDto {
                index,
                name,
                components: details.filename.to_vec().unwrap_or_default(),
                length: details.len,
                selected,
            });
        }

        let total_size = files.iter().map(|f| f.length).sum();
        let dto = TorrentPreviewDto {
            id: id.clone(),
            name: name.clone(),
            info_hash: list.info_hash.as_string(),
            total_size,
            files: files.clone(),
        };

        let job = TorrentJob {
            id: id.clone(),
            name,
            info_hash: dto.info_hash.clone(),
            torrent_bytes: list.torrent_bytes.to_vec(),
            files,
            status: TorrentJobStatus::Preview,
            output_dir,
            selected_files: Vec::new(),
            handle: None,
            error: None,
        };
        self.jobs.lock().await.insert(id, job);

        Ok(dto)
    }

    pub async fn status(&self, id: &str) -> anyhow::Result<TorrentJobDto> {
        let jobs = self.jobs.lock().await;
        let job = jobs.get(id).context("torrent job not found")?;
        Ok(job.dto())
    }

    pub async fn start(
        self: &Arc<Self>,
        id: &str,
        selected_files: Vec<usize>,
        inbox_dir: String,
    ) -> anyhow::Result<TorrentJobDto> {
        if selected_files.is_empty() {
            bail!("select at least one file");
        }
        if inbox_dir.trim().is_empty() {
            bail!("agent_inbox_dir is not configured");
        }
        let inbox_dir = validate_inbox_dir(&inbox_dir)?;

        let (torrent_bytes, output_dir) = {
            let mut jobs = self.jobs.lock().await;
            let job = jobs.get_mut(id).context("torrent job not found")?;
            if job.status != TorrentJobStatus::Preview && job.status != TorrentJobStatus::Failed {
                bail!("torrent job is already started");
            }
            validate_selection(&job.files, &selected_files)?;
            job.status = TorrentJobStatus::Downloading;
            job.selected_files = selected_files.clone();
            job.error = None;
            (job.torrent_bytes.clone(), job.output_dir.clone())
        };

        let session = self.session().await?;
        let response = session
            .add_torrent(
                AddTorrent::from_bytes(torrent_bytes),
                Some(AddTorrentOptions {
                    only_files: Some(selected_files),
                    output_folder: Some(output_dir.to_string_lossy().to_string()),
                    overwrite: true,
                    ..Default::default()
                }),
            )
            .await?;

        let handle = response
            .into_handle()
            .context("torrent did not return a download handle")?;

        let dto = {
            let mut jobs = self.jobs.lock().await;
            let job = jobs.get_mut(id).context("torrent job not found")?;
            job.handle = Some(handle.clone());
            job.dto()
        };

        let service = Arc::clone(self);
        let id = id.to_string();
        tokio::spawn(async move {
            if let Err(err) = handle.wait_until_completed().await {
                service.stop_torrent(&handle).await;
                service.fail_job(&id, err.to_string()).await;
                return;
            }
            service.stop_torrent(&handle).await;
            if let Err(err) = service.finalize_completed(&id, &inbox_dir).await {
                service.fail_job(&id, err.to_string()).await;
            }
        });

        Ok(dto)
    }

    async fn fail_job(&self, id: &str, error: String) {
        let mut jobs = self.jobs.lock().await;
        if let Some(job) = jobs.get_mut(id) {
            job.status = TorrentJobStatus::Failed;
            job.error = Some(error);
        }
    }

    async fn stop_torrent(&self, handle: &Arc<ManagedTorrent>) {
        match self.session().await {
            Ok(session) => {
                if let Err(err) = session.delete(handle.id().into(), false).await {
                    tracing::warn!("failed to stop completed torrent: {err}");
                }
            }
            Err(err) => {
                tracing::warn!("failed to access torrent session for shutdown: {err}");
            }
        }
    }

    async fn finalize_completed(&self, id: &str, inbox_dir: &Path) -> anyhow::Result<()> {
        let (name, files, selected_files, output_dir) = {
            let mut jobs = self.jobs.lock().await;
            let job = jobs.get_mut(id).context("torrent job not found")?;
            job.status = TorrentJobStatus::Moving;
            (
                job.name.clone(),
                job.files.clone(),
                job.selected_files.clone(),
                job.output_dir.clone(),
            )
        };

        let destination_root = inbox_dir
            .join("torrents")
            .join(sanitize_path_component(&name));
        tokio::fs::create_dir_all(&destination_root).await?;

        for file in files.iter().filter(|f| selected_files.contains(&f.index)) {
            let source = safe_join(&output_dir, &file.components)?;
            if !tokio::fs::try_exists(&source).await? {
                continue;
            }
            let destination = safe_join(&destination_root, &file.components)?;
            if let Some(parent) = destination.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            move_file(&source, &destination).await?;
        }

        let job_root = self.temp_root.join(id);
        let _ = tokio::fs::remove_dir_all(job_root).await;

        {
            let mut jobs = self.jobs.lock().await;
            let job = jobs.get_mut(id).context("torrent job not found")?;
            job.status = TorrentJobStatus::Complete;
        }

        if let Some(handle) = self.scheduler_handle.get() {
            let handle = Arc::clone(handle);
            tokio::spawn(async move {
                if let Err(err) = handle.trigger_job_now("inbox_discover").await {
                    tracing::warn!("failed to trigger inbox_discover after torrent: {err}");
                }
            });
        }

        Ok(())
    }
}

fn validate_selection(files: &[TorrentFileDto], selected_files: &[usize]) -> anyhow::Result<()> {
    for index in selected_files {
        if !files.iter().any(|file| file.index == *index) {
            bail!("selected file index {index} is not in this torrent");
        }
    }
    Ok(())
}

fn validate_inbox_dir(inbox_dir: &str) -> anyhow::Result<PathBuf> {
    let trimmed = inbox_dir.trim();
    let path = PathBuf::from(trimmed);
    if !path.is_absolute() {
        bail!(
            "agent_inbox_dir must be an absolute path for this host, got `{}`",
            trimmed
        );
    }
    Ok(path)
}

fn is_audio_path(path: &str) -> bool {
    let Some(ext) = Path::new(path).extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "mp3"
            | "flac"
            | "ogg"
            | "opus"
            | "aac"
            | "m4a"
            | "wav"
            | "ape"
            | "wv"
            | "wma"
            | "tta"
            | "aiff"
            | "aif"
    )
}

fn sanitize_path_component(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = sanitized.trim().trim_matches('.').trim();
    if trimmed.is_empty() {
        "torrent".to_string()
    } else {
        trimmed.to_string()
    }
}

fn safe_join(root: &Path, components: &[String]) -> anyhow::Result<PathBuf> {
    let mut path = root.to_path_buf();
    for component in components {
        let sanitized = sanitize_path_component(component);
        if sanitized == "." || sanitized == ".." {
            bail!("unsafe torrent path component");
        }
        path.push(sanitized);
    }
    Ok(path)
}

async fn move_file(source: &Path, destination: &Path) -> anyhow::Result<()> {
    match tokio::fs::rename(source, destination).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::CrossesDevices => {
            tokio::fs::copy(source, destination).await?;
            tokio::fs::remove_file(source).await?;
            Ok(())
        }
        Err(err) => Err(err.into()),
    }
}
