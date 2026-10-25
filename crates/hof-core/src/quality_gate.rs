//! Quality gate: hold a fresh video until the platform has processed the
//! resolution its profile asks for.
//!
//! `YouTube` publishes a video in low resolutions first and adds 1080p and
//! above once it has transcoded them, which can take hours for a long
//! upload. Downloading on discovery locks in whatever was ready at that
//! moment. The gate inspects the format list `download_video` fetches anyway,
//! and when the format selection would deliver less than the profile's height,
//! the worker returns [`crate::actors::download_worker::DownloadOutcome::Deferred`]
//! instead of downloading. The video then sits in `awaiting_quality` with
//! `next_retry` set to the recheck time.
//!
//! There is no platform signal for "higher resolutions are still coming", so
//! the gate cannot tell a video that is still processing from one that will
//! never be better. It bounds the cost of guessing wrong instead:
//!
//! - **Deadline.** The wait is measured from publication (first sighting when
//!   the platform gave no date), so a back-catalogue video found days after
//!   upload downloads at once, and nothing waits past `quality_wait_hours`.
//! - **Source history.** If every earlier download from this source topped out
//!   at or below what is available now, the channel does not upload at the
//!   profile's height and waiting would only delay it.

use chrono::{DateTime, Duration, Utc};

use crate::domain::profile::Quality;

/// Default for `QUALITY_WAIT_HOURS` when neither the environment nor the
/// profile sets one.
pub const DEFAULT_QUALITY_WAIT_HOURS: u32 = 24;

/// How long a held video waits between format checks.
pub const RECHECK_INTERVAL: Duration = Duration::hours(6);

/// The wait that applies to a profile.
///
/// Its own `quality_wait_hours`, or the global `QUALITY_WAIT_HOURS` when it
/// sets none. The column's `CHECK` keeps it non-negative; a value that still
/// fails to convert falls back too.
#[must_use]
pub fn effective_wait_hours(profile_override: Option<i32>, global: u32) -> u32 {
    profile_override
        .and_then(|h| u32::try_from(h).ok())
        .unwrap_or(global)
}

/// The pixel height a [`Quality`] asks for, if it names one.
///
/// `Best` and `AudioOnly` have no fixed target, so a shortfall is not
/// meaningful for them.
#[must_use]
pub const fn requested_height(quality: &Quality) -> Option<i32> {
    match quality {
        Quality::Q4320p => Some(4320),
        Quality::Q2160p => Some(2160),
        Quality::Q1440p => Some(1440),
        Quality::Q1080p => Some(1080),
        Quality::Q720p => Some(720),
        Quality::Q480p => Some(480),
        Quality::Best | Quality::AudioOnly => None,
    }
}

/// The inputs the gate needs for one video, resolved before the download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QualityGate {
    /// Height the profile asks for.
    pub target_height: i32,
    /// After this the video downloads at whatever is available.
    pub deadline: DateTime<Utc>,
    /// Highest height ever delivered for the video's source, if any.
    pub source_best_height: Option<i32>,
}

/// What the gate decided for one format check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    /// Download now.
    Download,
    /// Hold the video and check its formats again at `recheck_at`.
    Wait { recheck_at: DateTime<Utc> },
}

impl QualityGate {
    /// Build the gate for a video, or `None` when it does not apply.
    ///
    /// The gate is off when `wait_hours` is 0 or the quality names no height.
    /// `first_seen` stands in for `published_at` when the platform gave no
    /// date.
    #[must_use]
    pub fn new(
        quality: &Quality,
        wait_hours: u32,
        published_at: Option<DateTime<Utc>>,
        first_seen: DateTime<Utc>,
        source_best_height: Option<i32>,
    ) -> Option<Self> {
        if wait_hours == 0 {
            return None;
        }
        let target_height = requested_height(quality)?;
        let start = published_at.unwrap_or(first_seen);
        // An out-of-range wait saturates to "no deadline" rather than
        // disabling the gate; the history check still bounds it in practice.
        let deadline = start
            .checked_add_signed(Duration::hours(i64::from(wait_hours)))
            .unwrap_or(DateTime::<Utc>::MAX_UTC);

        Some(Self {
            target_height,
            deadline,
            source_best_height,
        })
    }

    /// Decide whether to download, given the height the format selection
    /// would deliver right now (`None` when no video format is listed yet).
    #[must_use]
    pub fn evaluate(&self, available_height: Option<i32>, now: DateTime<Utc>) -> GateDecision {
        if available_height.is_some_and(|h| h >= self.target_height) || now >= self.deadline {
            return GateDecision::Download;
        }

        // The source never delivered more than is on offer now: this is its
        // ceiling, not a transcode in progress.
        if let (Some(best), Some(available)) = (self.source_best_height, available_height)
            && available >= best
        {
            return GateDecision::Download;
        }

        let recheck_at = now
            .checked_add_signed(RECHECK_INTERVAL)
            .map_or(self.deadline, |t| t.min(self.deadline));
        GateDecision::Wait { recheck_at }
    }
}

#[cfg(test)]
mod tests {
    //! Failure modes, written down before the gate:
    //!
    //! 1. A video waits forever because better formats never appear.
    //! 2. A channel that never uploads at the target height makes every video
    //!    sit out the full wait.
    //! 3. A back-catalogue video, published long before it was indexed, waits
    //!    although its formats are final.
    //! 4. A video without a publish date never reaches a deadline.
    //! 5. `Best` / `AudioOnly` profiles, which name no height, get gated.
    //! 6. `quality_wait_hours = 0` does not switch the gate off.
    //! 7. The recheck is scheduled after the deadline, so the video downloads
    //!    late by up to one interval.
    //! 8. An empty format list (nothing processed yet) is treated as "good
    //!    enough" and downloads nothing useful.
    //! 9. A huge `quality_wait_hours` overflows the deadline and panics.
    //!
    //! Counting a deferral as a failed attempt (pushing the video to
    //! `permanently_failed`) and measuring the best format over all codecs
    //! instead of the preset's ladder are guarded outside this module, in
    //! `db::defer_video_for_quality` and `YtdlpClient::download_video`.

    use super::*;

    fn t(hours: i64) -> DateTime<Utc> {
        DateTime::<Utc>::UNIX_EPOCH + Duration::hours(1_000_000 + hours)
    }

    fn gate_2160(published_h: i64, source_best: Option<i32>) -> QualityGate {
        QualityGate::new(
            &Quality::Q2160p,
            24,
            Some(t(published_h)),
            t(published_h),
            source_best,
        )
        .unwrap_or_else(|| panic!("gate applies to 2160p"))
    }

    #[test]
    fn target_available_downloads() {
        assert_eq!(
            gate_2160(0, None).evaluate(Some(2160), t(1)),
            GateDecision::Download
        );
    }

    #[test]
    fn fresh_shortfall_waits_one_interval() {
        assert_eq!(
            gate_2160(0, None).evaluate(Some(1080), t(1)),
            GateDecision::Wait { recheck_at: t(7) }
        );
    }

    #[test]
    fn deadline_downloads_whatever_is_there() {
        // (1)
        assert_eq!(
            gate_2160(0, None).evaluate(Some(720), t(24)),
            GateDecision::Download
        );
    }

    #[test]
    fn source_ceiling_skips_the_wait() {
        // (2) channel never went above 1080p, 1080p is available now.
        assert_eq!(
            gate_2160(0, Some(1080)).evaluate(Some(1080), t(1)),
            GateDecision::Download
        );
    }

    #[test]
    fn source_history_above_available_still_waits() {
        // (2) channel has delivered 1440p before; only 720p is out yet.
        assert_eq!(
            gate_2160(0, Some(1440)).evaluate(Some(720), t(1)),
            GateDecision::Wait { recheck_at: t(7) }
        );
    }

    #[test]
    fn back_catalogue_video_downloads_at_once() {
        // (3) published 10 days before indexing.
        assert_eq!(
            gate_2160(-240, None).evaluate(Some(1080), t(0)),
            GateDecision::Download
        );
    }

    #[test]
    fn missing_publish_date_uses_first_sighting() {
        // (4)
        let gate = QualityGate::new(&Quality::Q1080p, 24, None, t(0), None)
            .unwrap_or_else(|| panic!("gate applies to 1080p"));
        assert_eq!(gate.deadline, t(24));
        assert_eq!(gate.evaluate(Some(480), t(25)), GateDecision::Download);
    }

    #[test]
    fn qualities_without_height_are_not_gated() {
        // (5)
        assert!(QualityGate::new(&Quality::Best, 24, Some(t(0)), t(0), None).is_none());
        assert!(QualityGate::new(&Quality::AudioOnly, 24, Some(t(0)), t(0), None).is_none());
    }

    #[test]
    fn zero_wait_disables_the_gate() {
        // (6)
        assert!(QualityGate::new(&Quality::Q2160p, 0, Some(t(0)), t(0), None).is_none());
    }

    #[test]
    fn recheck_never_lands_after_the_deadline() {
        // (7) 21h in, the next interval would end at 27h.
        assert_eq!(
            gate_2160(0, None).evaluate(Some(1080), t(21)),
            GateDecision::Wait { recheck_at: t(24) }
        );
    }

    #[test]
    fn no_video_formats_yet_waits() {
        // (8)
        assert_eq!(
            gate_2160(0, Some(1080)).evaluate(None, t(1)),
            GateDecision::Wait { recheck_at: t(7) }
        );
    }

    #[test]
    fn huge_wait_does_not_overflow() {
        // (9)
        let gate = QualityGate::new(&Quality::Q1080p, u32::MAX, Some(t(0)), t(0), None)
            .unwrap_or_else(|| panic!("gate applies to 1080p"));
        assert_eq!(
            gate.evaluate(Some(720), t(1)),
            GateDecision::Wait { recheck_at: t(7) }
        );
    }
}
