//! The `CleanupActor` is a singleton that runs on a periodic tick.
//! It enforces retention policies (source -> profile -> global precedence)
//! and storage quotas. Videos are only deleted when all referencing sources
//! agree the retention period has expired.
