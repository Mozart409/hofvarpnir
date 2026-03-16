//! The `SchedulerActor` is a singleton that fires indexing jobs on a per-source
//! schedule using `tokio::time`. On each tick it messages the appropriate
//! `SourceIndexerActor` to begin indexing.
