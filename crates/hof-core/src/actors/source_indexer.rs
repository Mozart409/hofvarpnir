//! The `SourceIndexerActor` is spawned per source. It calls
//! `yt-dlp --flat-playlist --dump-json` to discover new videos, filters them
//! by the source's cutoff date and profile settings (shorts, livestreams),
//! then sends `EnqueueDownload` messages to the `DownloadSupervisor`.

use std::sync::Arc;

use chrono::Utc;
use kameo::Reply;
use kameo::prelude::*;
use sqlx::PgPool;
use tracing::{debug, error, info, instrument, warn};
use ulid::Ulid;

use crate::db::{self, CreateVideo, UpdateChannelMetadata};
use crate::domain::profile::Profile;
use crate::domain::source::Source;
use crate::domain::video::VideoStatus;
use crate::jellyfin::{self, JellyfinMetadata};
use crate::ytdlp::{PlaylistEntry, VideoMetadata, YtdlpClient, YtdlpError};

use super::download_supervisor::{DownloadSupervisor, EnqueueDownload};

/// Result of indexing a source.
#[derive(Debug, Clone, Reply)]
pub struct IndexingResult {
    /// Source ID that was indexed.
    pub source_id: Ulid,
    /// Number of new videos discovered.
    pub new_videos: usize,
    /// Number of videos that already existed.
    pub existing_videos: usize,
    /// Number of videos filtered out (shorts, livestreams, before cutoff).
    pub filtered_out: usize,
    /// Any errors encountered (non-fatal).
    pub errors: Vec<String>,
}

/// The source indexer actor.
///
/// This actor handles indexing a single source to discover new videos.
/// It's typically short-lived, spawned by the scheduler when it's time
/// to index a source.
pub struct SourceIndexerActor {
    /// Database pool.
    pool: PgPool,
    /// yt-dlp client.
    ytdlp: Arc<YtdlpClient>,
    /// The source being indexed.
    source: Source,
    /// The profile this source belongs to.
    profile: Profile,
    /// Reference to the download supervisor for enqueueing downloads.
    supervisor: ActorRef<DownloadSupervisor>,
}

impl std::fmt::Debug for SourceIndexerActor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceIndexerActor")
            .field("source_id", &self.source.id)
            .field("source_url", &self.source.url)
            .finish_non_exhaustive()
    }
}

/// Arguments for spawning a source indexer.
pub struct SourceIndexerArgs {
    pub pool: PgPool,
    pub ytdlp: Arc<YtdlpClient>,
    pub source: Source,
    pub profile: Profile,
    pub supervisor: ActorRef<DownloadSupervisor>,
}

impl Actor for SourceIndexerActor {
    type Args = SourceIndexerArgs;
    type Error = color_eyre::eyre::Error;

    #[instrument(skip_all, fields(source_id = %args.source.id))]
    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        info!(
            source_id = %args.source.id,
            url = %args.source.url,
            "Source indexer starting"
        );

        let indexer = Self {
            pool: args.pool,
            ytdlp: args.ytdlp,
            source: args.source,
            profile: args.profile,
            supervisor: args.supervisor,
        };

        // Immediately start indexing.
        // Use try_send() to avoid potential deadlock from self-tell with bounded mailbox.
        if let Err(e) = actor_ref.tell(StartIndexing).try_send() {
            error!(error = %e, "Failed to start indexing");
            return Err(e.into());
        }

        Ok(indexer)
    }

    async fn on_stop(
        &mut self,
        _actor_ref: WeakActorRef<Self>,
        reason: ActorStopReason,
    ) -> Result<(), Self::Error> {
        debug!(
            source_id = %self.source.id,
            reason = ?reason,
            "Source indexer stopping"
        );
        Ok(())
    }
}

/// Message to start the indexing process.
pub struct StartIndexing;

impl Message<StartIndexing> for SourceIndexerActor {
    type Reply = IndexingResult;

    #[instrument(skip_all, fields(source_id = %self.source.id))]
    async fn handle(
        &mut self,
        _msg: StartIndexing,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let result = self.execute_indexing().await;

        // Stop the actor after indexing completes
        ctx.actor_ref().stop_gracefully().await.ok();

        result
    }
}

/// Message to manually trigger indexing (can be used for re-indexing).
pub struct IndexNow;

impl Message<IndexNow> for SourceIndexerActor {
    type Reply = Result<IndexingResult, String>;

    async fn handle(
        &mut self,
        _msg: IndexNow,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        Ok(self.execute_indexing().await)
    }
}

impl SourceIndexerActor {
    /// Execute the indexing process.
    #[allow(clippy::too_many_lines)]
    #[instrument(skip(self), fields(source_id = %self.source.id, url = %self.source.url))]
    async fn execute_indexing(&mut self) -> IndexingResult {
        let mut result = IndexingResult {
            source_id: self.source.id,
            new_videos: 0,
            existing_videos: 0,
            filtered_out: 0,
            errors: Vec::new(),
        };

        info!("Starting source indexing");

        // Fetch the playlist/channel
        let index_result = match self.ytdlp.index_source(&self.source.url).await {
            Ok(r) => r,
            Err(e) => {
                let error_msg = format!("Failed to index source: {e}");
                error!(error = %error_msg);
                result.errors.push(error_msg.clone());

                // Record the error in the database
                if let Err(db_err) =
                    db::record_source_indexing_error(&self.pool, self.source.id, &error_msg).await
                {
                    error!(error = %db_err, "Failed to record indexing error in database");
                }

                return result;
            }
        };

        info!(
            platform = %index_result.platform,
            title = %index_result.title,
            entries = index_result.entries.len(),
            "Source indexed successfully"
        );

        // Update channel metadata from index result
        if let Err(e) = self.update_channel_metadata(&index_result).await {
            warn!(error = %e, "Failed to update channel metadata");
            result
                .errors
                .push(format!("Channel metadata update failed: {e}"));
        }

        // Process each entry.
        // YouTube playlists are typically sorted newest-first, so we can stop early
        // once we hit several consecutive videos before the cutoff date.
        const MAX_CONSECUTIVE_BEFORE_CUTOFF: usize = 3;
        let mut consecutive_before_cutoff = 0;

        for entry in &index_result.entries {
            match self.process_entry(entry, &index_result.platform).await {
                EntryOutcome::New(video_id) => {
                    result.new_videos += 1;
                    consecutive_before_cutoff = 0; // Reset counter
                    // Enqueue for download
                    self.enqueue_video(video_id).await;
                }
                EntryOutcome::Existing => {
                    result.existing_videos += 1;
                    consecutive_before_cutoff = 0; // Reset counter
                }
                EntryOutcome::Filtered(reason) => {
                    debug!(
                        video_id = %entry.platform_video_id,
                        reason = %reason,
                        "Entry filtered out"
                    );
                    result.filtered_out += 1;
                    consecutive_before_cutoff = 0; // Not a cutoff filter
                }
                EntryOutcome::BeforeCutoff(reason) => {
                    debug!(
                        video_id = %entry.platform_video_id,
                        reason = %reason,
                        "Entry before cutoff date"
                    );
                    result.filtered_out += 1;
                    consecutive_before_cutoff += 1;

                    if consecutive_before_cutoff >= MAX_CONSECUTIVE_BEFORE_CUTOFF {
                        info!(
                            consecutive = consecutive_before_cutoff,
                            "Stopping early: found {} consecutive videos before cutoff date",
                            MAX_CONSECUTIVE_BEFORE_CUTOFF
                        );
                        break;
                    }
                }
                EntryOutcome::RateLimited(reason) => {
                    warn!(
                        video_id = %entry.platform_video_id,
                        reason = %reason,
                        "Rate limited, stopping indexing"
                    );
                    result.errors.push(reason.clone());
                    // Record error in database
                    if let Err(db_err) =
                        db::record_source_indexing_error(&self.pool, self.source.id, &reason).await
                    {
                        error!(error = %db_err, "Failed to record rate limit error");
                    }
                    // Stop indexing this source
                    break;
                }
                EntryOutcome::Error(e) => {
                    result.errors.push(e);
                    consecutive_before_cutoff = 0; // Reset on error
                }
            }
        }

        // Update last indexed timestamp
        if let Err(e) = db::update_source_last_indexed(&self.pool, self.source.id, Utc::now()).await
        {
            error!(error = %e, "Failed to update last_indexed_at");
            result
                .errors
                .push(format!("Failed to update last_indexed_at: {e}"));
        }

        // Generate Jellyfin metadata if needed (after we have channel metadata)
        // Reload source to get updated channel metadata
        if let Ok(updated_source) = db::get_source(&self.pool, self.source.id).await {
            // Temporarily update our source reference for metadata generation
            let indexer_with_updated_source = SourceIndexerActor {
                pool: self.pool.clone(),
                ytdlp: self.ytdlp.clone(),
                source: updated_source,
                profile: self.profile.clone(),
                supervisor: self.supervisor.clone(),
            };
            indexer_with_updated_source
                .generate_jellyfin_metadata_if_needed()
                .await;
        }

        info!(
            new = result.new_videos,
            existing = result.existing_videos,
            filtered = result.filtered_out,
            errors = result.errors.len(),
            "Indexing complete"
        );

        result
    }

    /// Process a single playlist entry.
    async fn process_entry(&self, entry: &PlaylistEntry, platform: &str) -> EntryOutcome {
        // First, check if we need to filter this entry based on title heuristics
        // (We can't do full filtering without fetching metadata for each video)
        if !self.profile.include_shorts && is_likely_short(entry) {
            return EntryOutcome::Filtered("shorts excluded".to_string());
        }

        // Check if video already exists in database
        match db::get_video_by_platform_id(&self.pool, platform, &entry.platform_video_id).await {
            Ok(existing) => {
                // Video exists, ensure it's linked to this source
                if let Err(e) =
                    db::link_video_to_source(&self.pool, self.source.id, existing.id).await
                {
                    return EntryOutcome::Error(format!("Failed to link video: {e}"));
                }
                EntryOutcome::Existing
            }
            Err(db::DbError::NotFound) => {
                // New video - need to fetch metadata to check date and other filters
                self.create_new_video(entry, platform).await
            }
            Err(e) => EntryOutcome::Error(format!("Database error: {e}")),
        }
    }

    /// Create a new video entry after fetching full metadata.
    async fn create_new_video(&self, entry: &PlaylistEntry, _platform: &str) -> EntryOutcome {
        // Fetch full metadata to get published date and other info
        let metadata = match self.ytdlp.fetch_video_metadata(&entry.url).await {
            Ok(m) => m,
            Err(YtdlpError::VideoUnavailable(msg)) => {
                return EntryOutcome::Filtered(format!("unavailable: {msg}"));
            }
            Err(YtdlpError::RateLimited(msg)) => {
                // Don't create videos when rate limited - we need proper metadata
                // to check cutoff dates. Return error to stop early.
                return EntryOutcome::RateLimited(format!("rate limited: {msg}"));
            }
            Err(e) => {
                // For transient errors, skip this video but continue indexing
                // Don't create videos without publish date since we can't enforce cutoff
                warn!(
                    video_id = %entry.platform_video_id,
                    error = %e,
                    "Failed to fetch metadata, skipping video"
                );
                return EntryOutcome::Filtered(format!("metadata fetch failed: {e}"));
            }
        };

        // Apply filters based on full metadata
        if let Some(filter_outcome) = self.check_video_filter(&metadata) {
            return filter_outcome;
        }

        // Create the video in the database
        self.create_video_from_metadata(&metadata).await
    }

    /// Check if a video should be filtered based on profile settings and cutoff date.
    /// Returns `None` if video should be included, or `Some(EntryOutcome)` if filtered.
    fn check_video_filter(&self, metadata: &VideoMetadata) -> Option<EntryOutcome> {
        // Check shorts
        if !self.profile.include_shorts && metadata.is_short() {
            return Some(EntryOutcome::Filtered("short video excluded".to_string()));
        }

        // Check livestreams
        if !self.profile.include_livestreams && (metadata.is_live || metadata.was_live) {
            return Some(EntryOutcome::Filtered("livestream excluded".to_string()));
        }

        // Check cutoff date - use BeforeCutoff variant for early termination detection
        if let Some(published) = metadata.published_at {
            let published_date = published.date_naive();
            if published_date < self.source.cutoff_date {
                return Some(EntryOutcome::BeforeCutoff(format!(
                    "published {} before cutoff {}",
                    published_date, self.source.cutoff_date
                )));
            }
        }

        None // Video should be included
    }

    /// Create a video from full metadata.
    async fn create_video_from_metadata(&self, metadata: &VideoMetadata) -> EntryOutcome {
        let create_video = CreateVideo {
            platform: &metadata.platform,
            platform_video_id: &metadata.platform_video_id,
            title: &metadata.title,
            description: metadata.description.as_deref(),
            duration_secs: metadata.duration_secs,
            published_at: metadata.published_at,
            thumbnail_url: metadata.thumbnail_url.as_deref(),
        };

        match db::upsert_video(&self.pool, create_video).await {
            Ok(video) => {
                // Link to this source
                if let Err(e) = db::link_video_to_source(&self.pool, self.source.id, video.id).await
                {
                    return EntryOutcome::Error(format!("Failed to link video: {e}"));
                }
                EntryOutcome::New(video.id)
            }
            Err(e) => EntryOutcome::Error(format!("Failed to create video: {e}")),
        }
    }

    /// Update channel metadata from indexing results.
    async fn update_channel_metadata(
        &self,
        index_result: &crate::ytdlp::IndexResult,
    ) -> Result<(), db::DbError> {
        let metadata = UpdateChannelMetadata {
            channel_id: index_result.channel_id.as_deref(),
            channel_title: Some(&index_result.title),
            channel_description: index_result.description.as_deref(),
            channel_thumbnail_url: index_result.thumbnail_url.as_deref(),
        };

        db::update_source_channel_metadata(&self.pool, self.source.id, metadata).await
    }

    /// Generate Jellyfin metadata files if not already generated.
    async fn generate_jellyfin_metadata_if_needed(&self) {
        // Check if we should generate metadata
        let output_dir = std::path::Path::new(&self.profile.output_dir).join("completed");

        // Check if metadata needs regeneration
        if !jellyfin::needs_regeneration(&output_dir, self.source.jellyfin_metadata_at, false) {
            debug!("Jellyfin metadata already exists, skipping generation");
            return;
        }

        info!("Generating Jellyfin metadata");

        // Build metadata from source
        let metadata = JellyfinMetadata::from_source(&self.source, "youtube");

        // Create HTTP client for image downloads
        let http_client = reqwest::Client::new();

        // Generate metadata files
        if let Err(e) = jellyfin::generate_metadata(&http_client, &metadata, &output_dir).await {
            warn!(error = %e, "Failed to generate Jellyfin metadata");
            return;
        }

        // Update timestamp in database
        if let Err(e) =
            db::update_source_jellyfin_metadata_at(&self.pool, self.source.id, Utc::now()).await
        {
            warn!(error = %e, "Failed to update Jellyfin metadata timestamp");
        }
    }

    /// Enqueue a video for download.
    async fn enqueue_video(&self, video_id: Ulid) {
        // Get the video from database
        let video = match db::get_video(&self.pool, video_id).await {
            Ok(v) => v,
            Err(e) => {
                error!(error = %e, video_id = %video_id, "Failed to get video for enqueueing");
                return;
            }
        };

        // Only enqueue if pending
        if video.status != VideoStatus::Pending {
            debug!(
                video_id = %video_id,
                status = ?video.status,
                "Video not pending, skipping enqueue"
            );
            return;
        }

        // Send to supervisor
        if let Err(e) = self
            .supervisor
            .tell(EnqueueDownload {
                video,
                profile: self.profile.clone(),
                source: self.source.clone(),
            })
            .await
        {
            error!(error = %e, video_id = %video_id, "Failed to enqueue download");
        }
    }
}

/// Outcome of processing a single entry.
enum EntryOutcome {
    /// New video was created.
    New(Ulid),
    /// Video already existed.
    Existing,
    /// Video was filtered out (not due to cutoff date).
    Filtered(String),
    /// Video was filtered because it's before the cutoff date.
    BeforeCutoff(String),
    /// Rate limited - should stop indexing this source.
    RateLimited(String),
    /// Error occurred.
    Error(String),
}

/// Heuristic check if an entry is likely a `YouTube` Short.
fn is_likely_short(entry: &PlaylistEntry) -> bool {
    // Common indicators in title
    let title_lower = entry.title.to_lowercase();
    if title_lower.contains("#shorts") || title_lower.contains("#short") {
        return true;
    }

    // Shorts are typically under 60 seconds
    if let Some(duration) = entry.duration_secs
        && duration <= 60
    {
        // Could be a short, but not definitive
        // We'll do a full check with metadata later
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_likely_short() {
        let short_entry = PlaylistEntry {
            platform_video_id: "abc123".to_string(),
            title: "My Cool Video #Shorts".to_string(),
            url: "https://youtube.com/watch?v=abc123".to_string(),
            duration_secs: Some(30),
            thumbnail_url: None,
        };
        assert!(is_likely_short(&short_entry));

        let regular_entry = PlaylistEntry {
            platform_video_id: "xyz789".to_string(),
            title: "A Regular Video Tutorial".to_string(),
            url: "https://youtube.com/watch?v=xyz789".to_string(),
            duration_secs: Some(600),
            thumbnail_url: None,
        };
        assert!(!is_likely_short(&regular_entry));
    }
}
