//! Source test builder.

use chrono::NaiveDate;
use hof_core::{
    db::{self, CreateSource},
    domain::source::{Source, SourceType},
};
use sqlx::PgPool;
use ulid::Ulid;

/// Builder for creating test sources.
pub struct SourceBuilder {
    profile_id: Ulid,
    url: String,
    source_type: SourceType,
    custom_name: Option<String>,
    enabled: bool,
    index_frequency_secs: i64,
    cutoff_date: NaiveDate,
    retention_days: Option<i32>,
}

impl SourceBuilder {
    /// Create a new source builder for a profile.
    #[must_use]
    pub fn new(profile_id: Ulid) -> Self {
        let id = Ulid::r#gen();
        Self {
            profile_id,
            url: format!("https://youtube.com/@test_channel_{id}"),
            source_type: SourceType::Channel,
            custom_name: None,
            enabled: true,
            index_frequency_secs: 3600,
            cutoff_date: NaiveDate::from_ymd_opt(2024, 1, 1).expect("valid date"),
            retention_days: None,
        }
    }

    /// Set the source URL.
    #[must_use]
    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }

    /// Set the source type.
    #[must_use]
    pub fn source_type(mut self, source_type: SourceType) -> Self {
        self.source_type = source_type;
        self
    }

    /// Set as a playlist.
    #[must_use]
    pub fn playlist(mut self) -> Self {
        self.source_type = SourceType::Playlist;
        self
    }

    /// Set a custom name.
    #[must_use]
    pub fn custom_name(mut self, name: impl Into<String>) -> Self {
        self.custom_name = Some(name.into());
        self
    }

    /// Disable the source.
    #[must_use]
    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    /// Set index frequency.
    #[must_use]
    pub fn index_frequency_secs(mut self, secs: i64) -> Self {
        self.index_frequency_secs = secs;
        self
    }

    /// Set cutoff date.
    #[must_use]
    pub fn cutoff_date(mut self, date: NaiveDate) -> Self {
        self.cutoff_date = date;
        self
    }

    /// Set retention days.
    #[must_use]
    pub fn retention_days(mut self, days: i32) -> Self {
        self.retention_days = Some(days);
        self
    }

    /// Build and insert the source into the database.
    pub async fn build(self, pool: &PgPool) -> Source {
        db::create_source(
            pool,
            CreateSource {
                profile_id: self.profile_id,
                url: &self.url,
                source_type: self.source_type,
                custom_name: self.custom_name.as_deref(),
                index_frequency_secs: self.index_frequency_secs,
                cutoff_date: self.cutoff_date,
                retention_days: self.retention_days,
            },
        )
        .await
        .expect("Failed to create test source")
    }
}
