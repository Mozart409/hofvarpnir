//! The `SourceIndexerActor` is spawned per source.
//!
//! It calls `yt-dlp --flat-playlist --dump-json` to discover new videos,
//! filters them by the source's cutoff date and profile settings (shorts,
//! livestreams), then sends `EnqueueDownload` messages to the
//! `DownloadSupervisor`.

use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use kameo::Reply;
use kameo::prelude::*;
use metrics::{counter, gauge, histogram};
use sqlx::PgPool;
use tracing::{debug, error, info, instrument, warn};
use ulid::Ulid;

use crate::db::{self, CreateVideo, UpdateChannelMetadata};
use crate::domain::profile::Profile;
use crate::domain::source::{EntryOrder, Source};
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
    /// Number of videos filtered because they are before cutoff date.
    pub filtered_before_cutoff: usize,
    /// Number of videos filtered as shorts.
    pub filtered_shorts: usize,
    /// Number of videos filtered as livestreams.
    pub filtered_livestreams: usize,
    /// Number of videos filtered because they are unavailable.
    pub filtered_unavailable: usize,
    /// Number of videos filtered because they are private.
    pub filtered_private: usize,
    /// Number of videos filtered for other reasons.
    pub filtered_other: usize,
    /// Any errors encountered (non-fatal).
    pub errors: Vec<String>,
}

impl IndexingResult {
    fn record_filtered(&mut self, reason: &str) {
        self.filtered_out += 1;

        let reason_lower = reason.to_lowercase();
        if reason_lower.contains("cutoff") {
            self.filtered_before_cutoff += 1;
        } else if reason_lower.contains("short") {
            self.filtered_shorts += 1;
        } else if reason_lower.contains("livestream") || reason_lower.contains("live stream") {
            self.filtered_livestreams += 1;
        } else if reason_lower.contains("private") {
            self.filtered_private += 1;
        } else if reason_lower.contains("unavailable") || reason_lower.contains("removed") {
            self.filtered_unavailable += 1;
        } else {
            self.filtered_other += 1;
        }
    }

    #[must_use]
    pub fn filtered_summary(&self) -> String {
        format!(
            "cutoff={}, shorts={}, livestreams={}, unavailable={}, private={}, other={}",
            self.filtered_before_cutoff,
            self.filtered_shorts,
            self.filtered_livestreams,
            self.filtered_unavailable,
            self.filtered_private,
            self.filtered_other
        )
    }
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
    /// Channel to send the indexing result back to the spawner.
    result_tx: Option<tokio::sync::oneshot::Sender<IndexingResult>>,
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
    /// Channel to send the indexing result back to the spawner.
    pub result_tx: tokio::sync::oneshot::Sender<IndexingResult>,
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
            result_tx: Some(args.result_tx),
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

        // Send result back to the scheduler via oneshot channel
        if let Some(tx) = self.result_tx.take()
            && tx.send(result.clone()).is_err()
        {
            warn!(source_id = %self.source.id, "Failed to send indexing result - receiver dropped");
        }

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
        let indexing_start = Instant::now();
        let mut result = IndexingResult {
            source_id: self.source.id,
            new_videos: 0,
            existing_videos: 0,
            filtered_out: 0,
            filtered_before_cutoff: 0,
            filtered_shorts: 0,
            filtered_livestreams: 0,
            filtered_unavailable: 0,
            filtered_private: 0,
            filtered_other: 0,
            errors: Vec::new(),
        };

        info!("Starting source indexing");

        // Fetch the playlist/channel
        let index_result = match self.ytdlp.index_source(&self.source.url).await {
            Ok(r) => r,
            Err(e) => {
                counter!(crate::metrics::SOURCE_INDEX_TOTAL, "status" => "error").increment(1);
                histogram!(crate::metrics::SOURCE_INDEX_DURATION_SECONDS)
                    .record(indexing_start.elapsed().as_secs_f64());
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
        debug!(
            channel_id = ?index_result.channel_id,
            channel_name = ?index_result.channel_name,
            thumbnail_url = ?index_result.thumbnail_url,
            "Updating channel metadata"
        );
        if let Err(e) = self.update_channel_metadata(&index_result).await {
            warn!(error = %e, "Failed to update channel metadata");
            result
                .errors
                .push(format!("Channel metadata update failed: {e}"));
        }

        // Detect entry order if unknown or if detection is stale (>30 days old)
        let needs_detection =
            self.source.entry_order == EntryOrder::Unknown || self.needs_order_redetection();

        let entry_order = if needs_detection {
            if self.source.entry_order != EntryOrder::Unknown {
                info!(
                    previous_order = ?self.source.entry_order,
                    detected_at = ?self.source.entry_order_detected_at,
                    "Re-detecting entry order (stale detection)"
                );
            }
            let detected = self.detect_entry_order(&index_result.entries).await;
            // Persist the detected order
            if let Err(e) =
                db::update_source_entry_order(&self.pool, self.source.id, detected).await
            {
                warn!(error = %e, "Failed to persist detected entry order");
            } else {
                info!(order = ?detected, "Detected and persisted entry order");
            }
            detected
        } else {
            self.source.entry_order
        };

        // Prepare entries based on detected order
        // For ascending order (oldest first), reverse to get newest first for cutoff logic
        let entries: Vec<&PlaylistEntry> = match entry_order {
            EntryOrder::Ascending => index_result.entries.iter().rev().collect(),
            _ => index_result.entries.iter().collect(),
        };

        // Early termination is only valid for ordered playlists
        let use_early_termination = entry_order != EntryOrder::Unordered;

        // Process each entry.
        // We stop early once we hit several consecutive videos before the cutoff date.
        const MAX_CONSECUTIVE_BEFORE_CUTOFF: usize = 3;
        let mut consecutive_before_cutoff = 0;

        info!(
            source_id = %self.source.id,
            source_type = ?self.source.source_type,
            platform = %index_result.platform,
            entry_order = ?entry_order,
            use_early_termination,
            "Configured entry processing strategy"
        );

        for entry in entries {
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
                    result.record_filtered(&reason);
                    consecutive_before_cutoff = 0; // Not a cutoff filter
                }
                EntryOutcome::BeforeCutoff(reason) => {
                    debug!(
                        video_id = %entry.platform_video_id,
                        reason = %reason,
                        "Entry before cutoff date"
                    );
                    result.record_filtered(&reason);
                    consecutive_before_cutoff += 1;

                    if use_early_termination
                        && consecutive_before_cutoff >= MAX_CONSECUTIVE_BEFORE_CUTOFF
                    {
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
            let indexer_with_updated_source = Self {
                pool: self.pool.clone(),
                ytdlp: self.ytdlp.clone(),
                source: updated_source,
                profile: self.profile.clone(),
                supervisor: self.supervisor.clone(),
                result_tx: None, // Not needed for metadata generation
            };
            indexer_with_updated_source
                .generate_jellyfin_metadata_if_needed()
                .await;
        }

        counter!(crate::metrics::SOURCE_INDEX_TOTAL, "status" => "success").increment(1);
        histogram!(crate::metrics::SOURCE_INDEX_DURATION_SECONDS)
            .record(indexing_start.elapsed().as_secs_f64());
        #[allow(clippy::cast_precision_loss)]
        gauge!(crate::metrics::SOURCE_INDEX_NEW_VIDEOS).set(result.new_videos as f64);

        info!(
            new = result.new_videos,
            existing = result.existing_videos,
            filtered = result.filtered_out,
            filtered_before_cutoff = result.filtered_before_cutoff,
            filtered_shorts = result.filtered_shorts,
            filtered_livestreams = result.filtered_livestreams,
            filtered_unavailable = result.filtered_unavailable,
            filtered_private = result.filtered_private,
            filtered_other = result.filtered_other,
            errors = result.errors.len(),
            "Indexing complete"
        );

        result
    }

    /// Process a single playlist entry.
    #[instrument(skip(self, entry), fields(video_id = %entry.platform_video_id))]
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
        &mut self,
        index_result: &crate::ytdlp::IndexResult,
    ) -> Result<(), db::DbError> {
        let metadata = UpdateChannelMetadata {
            channel_id: index_result.channel_id.as_deref(),
            channel_title: Some(&index_result.title),
            channel_description: index_result.description.as_deref(),
            channel_thumbnail_url: index_result.thumbnail_url.as_deref(),
        };

        db::update_source_channel_metadata(&self.pool, self.source.id, metadata).await?;

        if let Some(channel_id) = &index_result.channel_id {
            self.source.channel_id = Some(channel_id.clone());
        }
        self.source.channel_title = Some(index_result.title.clone());
        if let Some(description) = &index_result.description {
            self.source.channel_description = Some(description.clone());
        }
        if let Some(thumbnail_url) = &index_result.thumbnail_url {
            self.source.channel_thumbnail_url = Some(thumbnail_url.clone());
        }

        Ok(())
    }

    /// Generate Jellyfin metadata files if not already generated.
    async fn generate_jellyfin_metadata_if_needed(&self) {
        // Check if we should generate metadata
        let output_dir = self.source.completed_dir(&self.profile.output_dir);

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

    /// Check if entry order detection is stale and needs re-detection.
    fn needs_order_redetection(&self) -> bool {
        should_redetect_order(
            self.source.entry_order,
            self.source.entry_order_detected_at,
            Utc::now(),
        )
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

    /// Detect the entry order of a playlist by comparing publish dates of first and last entries.
    ///
    /// Returns:
    /// - `Ascending` if oldest entries come first
    /// - `Descending` if newest entries come first
    /// - `Unordered` if order cannot be determined (< 2 entries, missing dates, or equal dates)
    #[instrument(skip(self, entries), fields(entry_count = entries.len()))]
    async fn detect_entry_order(&self, entries: &[PlaylistEntry]) -> EntryOrder {
        // Need at least 2 entries to determine order
        if entries.len() < 2 {
            debug!("Cannot detect order: fewer than 2 entries");
            return EntryOrder::Unordered;
        }

        let first_entry = &entries[0];
        let last_entry = &entries[entries.len() - 1];

        debug!(
            first_video_id = %first_entry.platform_video_id,
            last_video_id = %last_entry.platform_video_id,
            "Fetching metadata to detect entry order"
        );

        // Fetch metadata for first entry
        let first_metadata = match self.ytdlp.fetch_video_metadata(&first_entry.url).await {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "Failed to fetch first entry metadata for order detection");
                return EntryOrder::Unordered;
            }
        };

        // Fetch metadata for last entry
        let last_metadata = match self.ytdlp.fetch_video_metadata(&last_entry.url).await {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "Failed to fetch last entry metadata for order detection");
                return EntryOrder::Unordered;
            }
        };

        // Compare publish dates
        let order =
            determine_order_from_dates(first_metadata.published_at, last_metadata.published_at);

        match order {
            EntryOrder::Ascending => {
                info!(
                    first_date = ?first_metadata.published_at,
                    last_date = ?last_metadata.published_at,
                    "Detected ascending order (oldest first)"
                );
            }
            EntryOrder::Descending => {
                info!(
                    first_date = ?first_metadata.published_at,
                    last_date = ?last_metadata.published_at,
                    "Detected descending order (newest first)"
                );
            }
            EntryOrder::Unordered => {
                debug!(
                    first_date = ?first_metadata.published_at,
                    last_date = ?last_metadata.published_at,
                    "Cannot determine order from dates"
                );
            }
            EntryOrder::Unknown => {
                // Should not happen from determine_order_from_dates
            }
        }

        order
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

/// Determine entry order from two publish dates (first and last entry).
///
/// Returns:
/// - `Ascending` if first < last (oldest first)
/// - `Descending` if first > last (newest first)
/// - `Unordered` if dates are equal or either is missing
fn determine_order_from_dates(
    first_date: Option<chrono::DateTime<Utc>>,
    last_date: Option<chrono::DateTime<Utc>>,
) -> EntryOrder {
    match (first_date, last_date) {
        (Some(first), Some(last)) if first < last => EntryOrder::Ascending,
        (Some(first), Some(last)) if first > last => EntryOrder::Descending,
        _ => EntryOrder::Unordered,
    }
}

/// Check if entry order detection should be re-run.
///
/// Re-detection is needed if:
/// - Entry order is not `Unknown` (already detected), AND
/// - Detection timestamp is `None` (legacy data) or older than 30 days
const REDETECTION_DAYS: i64 = 30;

fn should_redetect_order(
    entry_order: EntryOrder,
    detected_at: Option<chrono::DateTime<Utc>>,
    now: chrono::DateTime<Utc>,
) -> bool {
    if entry_order == EntryOrder::Unknown {
        return false;
    }

    match detected_at {
        None => true, // No timestamp means legacy data, re-detect
        Some(ts) => (now - ts).num_days() >= REDETECTION_DAYS,
    }
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
    use chrono::TimeZone;

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

    #[test]
    fn test_determine_order_ascending() {
        // First entry is older than last entry -> ascending (oldest first)
        let first = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
        let last = Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap();

        assert_eq!(
            determine_order_from_dates(Some(first), Some(last)),
            EntryOrder::Ascending
        );
    }

    #[test]
    fn test_determine_order_descending() {
        // First entry is newer than last entry -> descending (newest first)
        let first = Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap();
        let last = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();

        assert_eq!(
            determine_order_from_dates(Some(first), Some(last)),
            EntryOrder::Descending
        );
    }

    #[test]
    fn test_determine_order_equal_dates() {
        // Same date -> unordered
        let date = Utc.with_ymd_and_hms(2024, 3, 15, 12, 0, 0).unwrap();

        assert_eq!(
            determine_order_from_dates(Some(date), Some(date)),
            EntryOrder::Unordered
        );
    }

    #[test]
    fn test_determine_order_missing_first_date() {
        let last = Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap();

        assert_eq!(
            determine_order_from_dates(None, Some(last)),
            EntryOrder::Unordered
        );
    }

    #[test]
    fn test_determine_order_missing_last_date() {
        let first = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();

        assert_eq!(
            determine_order_from_dates(Some(first), None),
            EntryOrder::Unordered
        );
    }

    #[test]
    fn test_determine_order_both_dates_missing() {
        assert_eq!(
            determine_order_from_dates(None, None),
            EntryOrder::Unordered
        );
    }

    // ========================================================================
    // Re-detection tests
    // ========================================================================

    #[test]
    fn test_redetect_unknown_order_never_needs_redetection() {
        let now = Utc::now();
        // Unknown order should not trigger re-detection (it needs initial detection)
        assert!(!should_redetect_order(EntryOrder::Unknown, None, now));
        assert!(!should_redetect_order(EntryOrder::Unknown, Some(now), now));
    }

    #[test]
    fn test_redetect_no_timestamp_needs_redetection() {
        let now = Utc::now();
        // Detected order with no timestamp (legacy) should trigger re-detection
        assert!(should_redetect_order(EntryOrder::Ascending, None, now));
        assert!(should_redetect_order(EntryOrder::Descending, None, now));
        assert!(should_redetect_order(EntryOrder::Unordered, None, now));
    }

    #[test]
    fn test_redetect_fresh_detection_no_redetection() {
        let now = Utc::now();
        let detected_recently = now - chrono::Duration::days(10);
        // Detection within 30 days should not trigger re-detection
        assert!(!should_redetect_order(
            EntryOrder::Ascending,
            Some(detected_recently),
            now
        ));
    }

    #[test]
    fn test_redetect_stale_detection_needs_redetection() {
        let now = Utc::now();
        let detected_31_days_ago = now - chrono::Duration::days(31);
        // Detection older than 30 days should trigger re-detection
        assert!(should_redetect_order(
            EntryOrder::Descending,
            Some(detected_31_days_ago),
            now
        ));
    }

    #[test]
    fn test_redetect_exactly_30_days_needs_redetection() {
        let now = Utc::now();
        let detected_30_days_ago = now - chrono::Duration::days(30);
        // Exactly 30 days should trigger re-detection (>= 30)
        assert!(should_redetect_order(
            EntryOrder::Ascending,
            Some(detected_30_days_ago),
            now
        ));
    }

    #[test]
    fn test_redetect_29_days_no_redetection() {
        let now = Utc::now();
        let detected_29_days_ago = now - chrono::Duration::days(29);
        // 29 days should not trigger re-detection
        assert!(!should_redetect_order(
            EntryOrder::Ascending,
            Some(detected_29_days_ago),
            now
        ));
    }
}
