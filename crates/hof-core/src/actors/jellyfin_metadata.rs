//! The `JellyfinMetadataActor` runs daily to check and generate missing
//! Jellyfin metadata files (tvshow.nfo, poster.jpg, fanart.jpg) for sources.

use std::time::Duration;

use chrono::Utc;
use kameo::Reply;
use kameo::prelude::*;
use sqlx::PgPool;
use tracing::{debug, error, info, instrument, warn};

use crate::db;
use crate::jellyfin::{self, JellyfinMetadata};

/// Default interval for checking metadata (24 hours).
const DEFAULT_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Result of a metadata check cycle.
#[derive(Debug, Clone, Reply)]
pub struct MetadataCheckResult {
    /// Number of sources checked.
    pub sources_checked: usize,
    /// Number of sources with metadata generated.
    pub sources_generated: usize,
    /// Number of sources with existing metadata.
    pub sources_existing: usize,
    /// Errors encountered.
    pub errors: Vec<String>,
}

/// Status of the Jellyfin metadata actor.
#[derive(Debug, Clone, Reply)]
pub struct JellyfinMetadataStatus {
    /// Whether the actor is currently running a check.
    pub is_running: bool,
    /// Last check time.
    pub last_check_at: Option<chrono::DateTime<Utc>>,
    /// Next scheduled check.
    pub next_check_at: Option<chrono::DateTime<Utc>>,
}

/// The Jellyfin metadata actor.
///
/// This actor periodically checks all sources and generates missing
/// Jellyfin metadata files.
pub struct JellyfinMetadataActor {
    pool: PgPool,
    http_client: reqwest::Client,
    check_interval: Duration,
    is_running: bool,
    last_check_at: Option<chrono::DateTime<Utc>>,
}

impl std::fmt::Debug for JellyfinMetadataActor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JellyfinMetadataActor")
            .field("check_interval", &self.check_interval)
            .field("is_running", &self.is_running)
            .field("last_check_at", &self.last_check_at)
            .finish_non_exhaustive()
    }
}

/// Arguments for spawning the Jellyfin metadata actor.
pub struct JellyfinMetadataActorArgs {
    pub pool: PgPool,
    pub check_interval: Option<Duration>,
}

impl Actor for JellyfinMetadataActor {
    type Args = JellyfinMetadataActorArgs;
    type Error = color_eyre::eyre::Error;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        let check_interval = args.check_interval.unwrap_or(DEFAULT_CHECK_INTERVAL);

        info!(
            check_interval_secs = check_interval.as_secs(),
            "Jellyfin metadata actor starting"
        );

        let actor = Self {
            pool: args.pool,
            http_client: reqwest::Client::new(),
            check_interval,
            is_running: false,
            last_check_at: None,
        };

        // Schedule periodic checks
        actor_ref
            .tell(ScheduleNextCheck)
            .try_send()
            .map_err(|e| color_eyre::eyre::eyre!("Failed to schedule first check: {e}"))?;

        Ok(actor)
    }

    async fn on_stop(
        &mut self,
        _actor_ref: WeakActorRef<Self>,
        reason: ActorStopReason,
    ) -> Result<(), Self::Error> {
        info!(reason = ?reason, "Jellyfin metadata actor stopping");
        Ok(())
    }
}

/// Message to schedule the next check.
struct ScheduleNextCheck;

impl Message<ScheduleNextCheck> for JellyfinMetadataActor {
    type Reply = ();

    async fn handle(
        &mut self,
        _msg: ScheduleNextCheck,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        // Schedule the next check
        let actor_ref = ctx.actor_ref().clone();
        let interval = self.check_interval;

        tokio::spawn(async move {
            tokio::time::sleep(interval).await;
            if let Err(e) = actor_ref.tell(RunCheck).try_send() {
                error!(error = %e, "Failed to trigger metadata check");
            }
        });
    }
}

/// Message to run a metadata check now.
pub struct RunCheck;

impl Message<RunCheck> for JellyfinMetadataActor {
    type Reply = MetadataCheckResult;

    #[instrument(skip_all)]
    async fn handle(
        &mut self,
        _msg: RunCheck,
        ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        if self.is_running {
            debug!("Metadata check already running, skipping");
            return MetadataCheckResult {
                sources_checked: 0,
                sources_generated: 0,
                sources_existing: 0,
                errors: vec!["Check already in progress".to_string()],
            };
        }

        self.is_running = true;
        let result = self.check_all_sources().await;
        self.is_running = false;
        self.last_check_at = Some(Utc::now());

        // Schedule next check
        ctx.actor_ref().tell(ScheduleNextCheck).try_send().ok();

        result
    }
}

/// Message to get the current status.
pub struct GetStatus;

impl Message<GetStatus> for JellyfinMetadataActor {
    type Reply = JellyfinMetadataStatus;

    async fn handle(
        &mut self,
        _msg: GetStatus,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        JellyfinMetadataStatus {
            is_running: self.is_running,
            last_check_at: self.last_check_at,
            next_check_at: self
                .last_check_at
                .map(|t| t + chrono::Duration::from_std(self.check_interval).unwrap_or_default()),
        }
    }
}

impl JellyfinMetadataActor {
    /// Check all sources and generate missing metadata.
    async fn check_all_sources(&self) -> MetadataCheckResult {
        let mut result = MetadataCheckResult {
            sources_checked: 0,
            sources_generated: 0,
            sources_existing: 0,
            errors: Vec::new(),
        };

        info!("Starting Jellyfin metadata check for all sources");

        // Get all sources
        let sources = match db::list_sources(&self.pool).await {
            Ok(s) => s,
            Err(e) => {
                let error = format!("Failed to list sources: {e}");
                error!(error = %error);
                result.errors.push(error);
                return result;
            }
        };

        // Get profiles for output directory lookup
        let profiles = match db::list_profiles(&self.pool).await {
            Ok(p) => p,
            Err(e) => {
                let error = format!("Failed to list profiles: {e}");
                error!(error = %error);
                result.errors.push(error);
                return result;
            }
        };

        for source in sources {
            result.sources_checked += 1;

            // Find the profile for this source
            let Some(profile) = profiles.iter().find(|p| p.id == source.profile_id) else {
                warn!(source_id = %source.id, "Profile not found for source");
                continue;
            };

            // Determine output directory
            let output_dir = std::path::Path::new(&profile.output_dir).join("completed");

            // Check if metadata needs regeneration
            if !jellyfin::needs_regeneration(&output_dir, source.jellyfin_metadata_at, false) {
                debug!(source_id = %source.id, "Metadata already exists");
                result.sources_existing += 1;
                continue;
            }

            // Skip sources without channel metadata
            if source.channel_title.is_none() && source.custom_name.is_none() {
                debug!(source_id = %source.id, "No channel metadata, skipping");
                continue;
            }

            // Generate metadata
            let metadata = JellyfinMetadata::from_source(&source, "youtube");

            match jellyfin::generate_metadata(&self.http_client, &metadata, &output_dir).await {
                Ok(()) => {
                    result.sources_generated += 1;

                    // Update timestamp
                    if let Err(e) =
                        db::update_source_jellyfin_metadata_at(&self.pool, source.id, Utc::now())
                            .await
                    {
                        warn!(error = %e, source_id = %source.id, "Failed to update timestamp");
                    }
                }
                Err(e) => {
                    let error = format!("Source {}: {e}", source.id);
                    warn!(error = %error);
                    result.errors.push(error);
                }
            }
        }

        info!(
            checked = result.sources_checked,
            generated = result.sources_generated,
            existing = result.sources_existing,
            errors = result.errors.len(),
            "Jellyfin metadata check complete"
        );

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_check_interval() {
        assert_eq!(DEFAULT_CHECK_INTERVAL.as_secs(), 24 * 60 * 60);
    }
}
