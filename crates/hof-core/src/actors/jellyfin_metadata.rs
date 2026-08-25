//! The `JellyfinMetadataActor` runs daily to check and generate missing
//! Jellyfin metadata files (tvshow.nfo, poster.jpg, fanart.jpg) for sources.

use std::time::Duration;

use chrono::Utc;
use kameo::Reply;
use kameo::prelude::*;
use sqlx::PgPool;
use tracing::{debug, error, info, instrument, warn};

use ulid::Ulid;

use crate::db;
use crate::db::ActivityBroadcaster;
use crate::domain::activity::{ActivityEventType, ActivitySeverity};
use crate::jellyfin::{self, JellyfinMetadata};

/// Default interval for checking metadata (24 hours).
const DEFAULT_CHECK_INTERVAL: Duration = Duration::from_hours(24);

/// How long a self-reschedule wait for mailbox space before giving up.
/// Unlike the scheduler/cleanup actors, this actor has no recurring tick
/// loop — each cycle reschedules itself with a single message send. A
/// bare `try_send()` failure here (mailbox momentarily full) used to end
/// the periodic Jellyfin metadata check permanently, with no recovery
/// and, for the post-check reschedule, no log at all (`.ok()`).
const RESCHEDULE_SEND_TIMEOUT: Duration = Duration::from_secs(10);

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
    broadcaster: ActivityBroadcaster,
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
    pub broadcaster: ActivityBroadcaster,
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
            broadcaster: args.broadcaster,
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
            send_self_with_retry(&actor_ref, || RunCheck, "RunCheck").await;
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

        // Schedule next check. This is a self-tell, so it must not block
        // waiting on the mailbox from inside this handler (the actor can't
        // drain its own mailbox while stuck in this very handler — a real
        // deadlock risk, unlike a tick loop on a separate task). Hand off to
        // a spawned task, which can then safely wait/retry for mailbox space.
        let actor_ref = ctx.actor_ref().clone();
        tokio::spawn(async move {
            send_self_with_retry(&actor_ref, || ScheduleNextCheck, "ScheduleNextCheck").await;
        });

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

/// Message to trigger metadata generation for a specific source.
pub struct TriggerSourceMetadata {
    /// The source ID to generate metadata for.
    pub source_id: Ulid,
}

/// Result of triggering metadata for a single source.
#[derive(Debug, Clone, Reply)]
pub struct SourceMetadataResult {
    /// Whether metadata was generated successfully.
    pub success: bool,
    /// Error message if generation failed.
    pub error: Option<String>,
}

impl Message<TriggerSourceMetadata> for JellyfinMetadataActor {
    type Reply = SourceMetadataResult;

    #[instrument(skip_all, fields(source_id = %msg.source_id))]
    async fn handle(
        &mut self,
        msg: TriggerSourceMetadata,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        match self.generate_source_metadata(msg.source_id).await {
            Ok(()) => SourceMetadataResult {
                success: true,
                error: None,
            },
            Err(e) => {
                let error_msg = e.to_string();
                warn!(error = %error_msg, "Failed to generate source metadata");
                SourceMetadataResult {
                    success: false,
                    error: Some(error_msg),
                }
            }
        }
    }
}

impl JellyfinMetadataActor {
    /// Generate metadata for a specific source.
    #[instrument(skip(self), fields(source_id = %source_id))]
    async fn generate_source_metadata(&self, source_id: Ulid) -> color_eyre::eyre::Result<()> {
        let source = db::get_source(&self.pool, source_id)
            .await
            .map_err(|e| color_eyre::eyre::eyre!("Failed to get source: {e}"))?;

        let profile = db::get_profile(&self.pool, source.profile_id)
            .await
            .map_err(|e| color_eyre::eyre::eyre!("Failed to get profile: {e}"))?;

        // Warn if source lacks channel metadata
        if source.channel_thumbnail_url.is_none() {
            warn!(
                source_id = %source_id,
                "Source has no channel thumbnail URL - run 'Trigger Index' first to fetch channel metadata"
            );
        }

        let output_dir = source.completed_dir(&profile.output_dir);

        let metadata = JellyfinMetadata::from_source(&source, "youtube");

        jellyfin::generate_metadata(&self.http_client, &metadata, &output_dir).await?;

        db::update_source_jellyfin_metadata_at(&self.pool, source_id, Utc::now())
            .await
            .map_err(|e| color_eyre::eyre::eyre!("Failed to update metadata timestamp: {e}"))?;

        info!(source_id = %source_id, "Generated Jellyfin metadata for source");

        let message = format!(
            "Generated Jellyfin metadata for \"{}\"",
            source.display_name()
        );
        self.broadcaster
            .log_and_broadcast(
                &self.pool,
                ActivityEventType::MetadataGenerated,
                ActivitySeverity::Info,
                &message,
                Some(source_id),
                None,
                None,
            )
            .await;

        Ok(())
    }

    /// Check all sources and generate missing metadata.
    #[instrument(skip(self))]
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

            // Skip sources without channel metadata
            if source.channel_title.is_none() && source.custom_name.is_none() {
                debug!(source_id = %source.id, "No channel metadata, skipping");
                continue;
            }

            let output_dir = source.completed_dir(&profile.output_dir);

            // Check if metadata needs regeneration
            if !jellyfin::needs_regeneration(&output_dir, source.jellyfin_metadata_at, false) {
                debug!(source_id = %source.id, "Metadata already exists");
                result.sources_existing += 1;
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

/// Sends a self-message with a bounded retry if the mailbox is momentarily
/// full, logging loudly if all attempts are exhausted. Unlike the
/// scheduler/cleanup actors' recurring tick loops, this actor has no outer
/// loop to fall back on — a give-up here permanently stops periodic
/// Jellyfin metadata generation until the process restarts, so it retries
/// harder before surrendering.
///
/// Must only be called from a task that is *not* the actor's own message
/// handler execution — see the call site in `RunCheck` for why.
async fn send_self_with_retry<M>(
    actor_ref: &ActorRef<JellyfinMetadataActor>,
    mut make_msg: impl FnMut() -> M,
    message_name: &str,
) where
    JellyfinMetadataActor: Message<M>,
    M: Send + 'static,
{
    const MAX_ATTEMPTS: u32 = 3;
    const SEND_TIMEOUT: Duration = RESCHEDULE_SEND_TIMEOUT;

    for attempt in 1..=MAX_ATTEMPTS {
        match actor_ref
            .tell(make_msg())
            .mailbox_timeout(SEND_TIMEOUT)
            .send()
            .await
        {
            Ok(()) | Err(SendError::ActorNotRunning(_)) => return,
            Err(e) if attempt < MAX_ATTEMPTS => {
                warn!(error = %e, attempt, message = message_name, "Mailbox busy, retrying");
            }
            Err(e) => {
                error!(
                    error = %e,
                    message = message_name,
                    "Failed to send after retries — periodic Jellyfin metadata checks have stopped until restart"
                );
            }
        }
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
