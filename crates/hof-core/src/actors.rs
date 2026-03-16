//! Actor system for managing downloads, indexing, and cleanup.
//!
//! This module contains the Kameo actors that drive the application:
//!
//! - [`DownloadWorker`]: Short-lived actor for downloading a single video
//! - [`DownloadSupervisor`]: Singleton managing download concurrency and retries
//! - [`SourceIndexerActor`]: Per-source actor for discovering videos
//! - [`SchedulerActor`]: Singleton that triggers indexing on schedule
//! - [`CleanupActor`]: Singleton enforcing retention policies and quotas

pub mod cleanup;
pub mod download_supervisor;
pub mod download_worker;
pub mod scheduler;
pub mod source_indexer;

// Re-export commonly used types
pub use cleanup::{CleanupActor, CleanupActorArgs, CleanupResult, CleanupStatus};
pub use download_supervisor::{
    DownloadSupervisor, DownloadSupervisorArgs, EnqueueDownload, SupervisorStatus,
};
pub use download_worker::{DownloadConfig, DownloadOutcome, DownloadWorker, DownloadWorkerArgs};
pub use scheduler::{SchedulerActor, SchedulerArgs, SchedulerStatus};
pub use source_indexer::{IndexingResult, SourceIndexerActor, SourceIndexerArgs};
