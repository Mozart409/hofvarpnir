//! End-to-end download pipeline tests.
//!
//! These drive a real `DownloadWorker` against a real `yt-dlp` binary and a
//! locally served media file, and assert on persisted state plus what is left
//! on disk — not just on a status code.
//!
//! # Why these stop at the format-selection stage
//!
//! The original intent was to cover the full happy path
//! (index -> download -> verify -> `completed/`). A locally served file cannot
//! reach it, for two independent reasons — and **neither is a bug**:
//!
//! 1. A locally served file cannot be a *source* at all. `Source` is only ever
//!    `Channel` or `Playlist`, and `YtdlpClient::index_source` indexes it with
//!    `fetch_playlist`. `yt-dlp` answers a direct media URL with
//!    `_type: video` and no `entries`, so there is nothing to enumerate.
//!    That is the product's data model, not a defect.
//! 2. Even reaching the worker directly, as these tests do, selection finds
//!    nothing. The `generic` extractor reports a direct URL as one format with
//!    `vcodec: null` and `acodec: null` — it does not probe the file — and
//!    `Format::format_type` classifies a format with neither codec as
//!    `FormatType::Unknown`, so it is neither `is_video()` nor `is_audio()`.
//!
//! Do **not** "fix" (2) by teaching the fork to classify `direct: true`
//! formats by extension. That changes how every real download picks a format,
//! on a vendored code path, to serve a test — and it would still not make (1)
//! work, so it buys no user-facing behavior. It was considered and rejected.
//!
//! If full happy-path coverage is wanted later, the honest shape is a
//! network-gated test against a real platform (env-gated or `#[ignore]`d so CI
//! stays hermetic), not a local fixture.
//!
//! So these tests pin what *is* reachable end-to-end: the error contract and
//! the guarantee that a failed download leaves no partial media behind. The
//! stages past selection are covered elsewhere — the codec ladder by
//! `ytdlp::tests` in `crates/hof-core/src/ytdlp.rs`, and the verification gate
//! by `crates/hof-core/src/verify.rs`'s own tests, which generate real clips
//! with ffmpeg and assert on truncation, interior zero runs and missing
//! streams.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use hof_core::{db, domain::profile::OutputPreset, domain::video::VideoStatus};
use sqlx::PgPool;
use tempfile::TempDir;
use tokio::time::sleep;
use ulid::Ulid;
use wiremock::matchers::path;
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::helpers::{ProfileBuilder, SourceBuilder, TestApp, UserBuilder};

/// Generate a small test clip with ffmpeg.
///
/// Uses `mpeg4`/`aac`, which are built into every ffmpeg, so this needs no
/// codec licensing and matches `crates/hof-core/src/verify.rs`'s own fixtures.
fn make_test_clip(path: &Path, duration_secs: u32) {
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-f", "lavfi", "-i"])
        .arg(format!("testsrc=d={duration_secs}:s=160x120:r=15"))
        .args(["-f", "lavfi", "-i"])
        .arg(format!("sine=d={duration_secs}"))
        .args([
            "-c:v",
            "mpeg4",
            "-c:a",
            "aac",
            "-movflags",
            "+faststart",
            "-y",
        ])
        .arg(path)
        .status()
        .expect("ffmpeg must be available to run download pipeline tests");

    assert!(
        status.success(),
        "ffmpeg failed to generate {}",
        path.display()
    );
}

/// Every media file under `dir`, recursively.
///
/// Used to prove a failed download left nothing behind: the segmented
/// downloader resumes onto an existing output file and decides a segment is
/// complete by probing only its first and last bytes, so a stray partial file
/// poisons every later retry.
fn media_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };

    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            found.extend(media_files(&path));
        } else if path
            .extension()
            .is_some_and(|ext| matches!(ext.to_str(), Some("mp4" | "mkv" | "webm" | "part")))
        {
            found.push(path);
        }
    }

    found
}

/// Poll the database until `video_id` leaves `Pending`/`Downloading`.
///
/// Reads the row directly rather than the API so the assertion is about what
/// was persisted, and returns the terminal status with its error text.
async fn await_terminal_status(
    pool: &PgPool,
    video_id: Ulid,
    timeout: Duration,
) -> (VideoStatus, Option<String>) {
    let deadline = std::time::Instant::now() + timeout;

    loop {
        let video = db::get_video(pool, video_id)
            .await
            .expect("video should exist");

        if !matches!(
            video.status,
            VideoStatus::Pending | VideoStatus::Downloading
        ) {
            return (video.status, video.last_error);
        }

        assert!(
            std::time::Instant::now() < deadline,
            "video {video_id} never left Pending/Downloading within {timeout:?}"
        );
        sleep(Duration::from_millis(250)).await;
    }
}

/// Seed a source and a video pointing at `url`, then enqueue the download.
///
/// Returns the video id.
async fn enqueue_url_download(
    app: &TestApp,
    pool: &PgPool,
    output_dir: &Path,
    url: &str,
    preset: OutputPreset,
) -> Ulid {
    let user = UserBuilder::new().build(pool).await;

    let profile = ProfileBuilder::new(user.id)
        .output_preset(preset)
        .output_dir(output_dir.to_string_lossy().to_string())
        .build(pool)
        .await;

    let source = SourceBuilder::new(profile.id)
        .url(url)
        .custom_name("Local Clip Source")
        .build(pool)
        .await;

    let video = db::create_video(
        pool,
        db::CreateVideo {
            platform: "generic",
            // `DownloadWorker::build_video_url` uses `platform_video_id`
            // verbatim as the URL when it starts with `http` (the catch-all
            // arm), so the served URL has to live here — a bare id would be
            // expanded into `https://generic.com/<id>`.
            platform_video_id: url,
            title: "Local Clip",
            description: Some("Served by wiremock for the download pipeline tests"),
            duration_secs: Some(3),
            published_at: None,
            thumbnail_url: None,
        },
    )
    .await
    .expect("create video");

    db::link_video_to_source(pool, source.id, video.id)
        .await
        .expect("link video to source");

    app.enqueue_download(video.id, source.id).await;

    video.id
}

/// A download whose format ladder is exhausted must report the machine-readable
/// error code, persist it on the row, and leave no partial media on disk.
///
/// This is the medium-hard case rather than a bare "returns an error": the
/// clip is real and reachable and the metadata parse succeeds, so the failure
/// happens late, at format selection, and the test proves the worker cleans up
/// after a late-stage failure rather than only that it reports one.
///
/// It also pins the `generic` extractor's metadata parse, which the three
/// `#[serde(default)]` attributes in section 1b of
/// `patches/yt-dlp-patched/PATCHES.md` are what make succeed. Drop those on a
/// re-sync and this test fails on the metadata error instead of reaching the
/// ladder.
#[sqlx::test(migrations = "../hof-core/migrations")]
async fn format_unavailable_is_reported_and_leaves_no_partial_file(pool: PgPool) {
    let download_dir = TempDir::new().expect("create temp dir");
    let download_path = download_dir.path();

    let clip_path = download_path.join("fixture.mp4");
    make_test_clip(&clip_path, 3);
    let clip_bytes = std::fs::read(&clip_path).expect("read clip bytes");
    std::fs::remove_file(&clip_path).expect("remove fixture from the download dir");

    let mock_server = MockServer::start().await;
    Mock::given(path("/clip.mp4"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(clip_bytes)
                .insert_header("content-type", "video/mp4"),
        )
        .mount(&mock_server)
        .await;

    // Verification on, so a file that somehow got downloaded would still be
    // gated rather than silently published.
    let app = TestApp::with_verification(pool.clone(), true).await;

    let url = format!("{}/clip.mp4", mock_server.uri());
    let video_id = enqueue_url_download(&app, &pool, download_path, &url, OutputPreset::Auto).await;

    let (status, last_error) =
        await_terminal_status(&pool, video_id, Duration::from_secs(90)).await;

    assert_eq!(
        status,
        VideoStatus::Failed,
        "a source yt-dlp cannot pick a format for must fail, not hang or complete"
    );

    let last_error = last_error.expect("a failed download must record why");
    assert!(
        last_error.contains(hof_core::ytdlp::YtdlpError::DOWNLOAD_FORMAT_UNAVAILABLE),
        "failure must carry the machine-readable code, got: {last_error}"
    );

    // The real artifact: nothing partial survived the failure anywhere under
    // the profile's output directory.
    let leftovers = media_files(download_path);
    assert!(
        leftovers.is_empty(),
        "a failed download must leave no media behind, found: {leftovers:?}"
    );

    // And the delivered-quality columns stay empty — they record what was
    // actually served, so a failure must not populate them.
    let video = db::get_video(&pool, video_id)
        .await
        .expect("video should exist");
    assert_eq!(video.video_height, None, "no height should be recorded");
    assert_eq!(video.video_codec, None, "no codec should be recorded");
}

/// A source whose URL does not resolve at all fails with the execution code
/// rather than the format code, so the two failure modes stay distinguishable
/// in `last_error` and in the API's `last_error_code`.
#[sqlx::test(migrations = "../hof-core/migrations")]
async fn unreachable_source_fails_with_execution_error(pool: PgPool) {
    let download_dir = TempDir::new().expect("create temp dir");
    let download_path = download_dir.path();

    // Bind a mock server only to take its address, then drop it so the port
    // is closed. Any request to it now fails to connect.
    let dead_url = {
        let server = MockServer::start().await;
        format!("{}/gone.mp4", server.uri())
    };

    let app = TestApp::with_verification(pool.clone(), true).await;

    let video_id =
        enqueue_url_download(&app, &pool, download_path, &dead_url, OutputPreset::Auto).await;

    let (status, last_error) =
        await_terminal_status(&pool, video_id, Duration::from_secs(90)).await;

    assert_eq!(status, VideoStatus::Failed, "an unreachable URL must fail");

    let last_error = last_error.expect("a failed download must record why");
    assert!(
        !last_error.contains(hof_core::ytdlp::YtdlpError::DOWNLOAD_FORMAT_UNAVAILABLE),
        "an unreachable URL is not a format problem, got: {last_error}"
    );

    let leftovers = media_files(download_path);
    assert!(
        leftovers.is_empty(),
        "a failed download must leave no media behind, found: {leftovers:?}"
    );
}
