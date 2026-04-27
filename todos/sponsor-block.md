# SponsorBlock Integration Plan

Skip or mark sponsor segments in downloaded YouTube videos using the SponsorBlock API.

## Current Architecture

Hofvarpnir uses the `yt-dlp` Rust crate (v2.7.2) which has a split architecture:

| Operation | Method | CLI Args Support |
|-----------|--------|------------------|
| Metadata extraction | Shells out to yt-dlp CLI | Yes (`with_arg()`) |
| Video downloads | Pure Rust HTTP engine | No |

The `DownloadBuilder` only supports: video/audio quality, codec preferences, priority, progress callbacks. **No arbitrary CLI args like `--sponsorblock-*`**.

---

## Goals

- Allow users to skip or remove sponsor segments from YouTube videos
- Support configurable SponsorBlock categories (sponsor, intro, outro, selfpromo, etc.)
- Maintain compatibility with non-YouTube sources (SponsorBlock is YouTube-only)
- Minimal impact on download performance

---

## SponsorBlock Categories

Available via SponsorBlock API:

| Category | Description |
|----------|-------------|
| `sponsor` | Paid promotion, paid referrals, direct advertisements |
| `intro` | Intermission/intro animation |
| `outro` | Credits, end cards |
| `selfpromo` | Unpaid/self promotion |
| `preview` | Preview/recap of upcoming content |
| `filler` | Tangential filler content |
| `interaction` | "Like and subscribe" reminders |
| `music_offtopic` | Non-music section of music video |

---

## Integration Options

### Option 1: Post-Download Processing (Recommended)

Fetch segment data during metadata extraction, then use ffmpeg to remove segments after download.

**Pros:**
- Works with current architecture
- No changes to download logic
- Can be toggled per-profile

**Cons:**
- Re-encoding required (or complex stream copying)
- Slight increase in processing time
- Requires storing segment data

**Implementation:**

1. During metadata fetch, request SponsorBlock segments via yt-dlp:
   ```rust
   extractor.with_arg("--sponsorblock-mark=all".to_string());
   ```

2. Parse chapters from video metadata (yt-dlp embeds SponsorBlock as chapters)

3. After download, use ffmpeg to either:
   - Remove segments entirely (`--sponsorblock-remove`)
   - Keep chapters marked (no re-encode needed)

### Option 2: CLI-Based Downloads

Replace the Rust crate's download method with direct yt-dlp CLI invocation.

**Pros:**
- Full access to all yt-dlp features
- Native `--sponsorblock-remove` support
- Simpler implementation

**Cons:**
- Lose Rust crate's download optimizations
- More subprocess management
- Harder to track progress

### Option 3: Upstream Contribution

Add SponsorBlock support to the `yt-dlp` Rust crate.

**Pros:**
- Benefits entire Rust ecosystem
- Clean integration

**Cons:**
- Significant effort
- Dependent on upstream acceptance
- Long timeline

---

## Recommended Approach: Option 1

### Phase 1: Database Schema

```sql
-- Profile-level SponsorBlock settings
ALTER TABLE profiles ADD COLUMN sponsorblock_mode TEXT DEFAULT 'disabled';
-- Values: 'disabled', 'mark', 'remove'

ALTER TABLE profiles ADD COLUMN sponsorblock_categories TEXT[] DEFAULT ARRAY['sponsor'];
-- Array of category names to handle
```

### Phase 2: Metadata Extraction

Modify `YtdlpClient::fetch_video_metadata()` to request SponsorBlock data:

```rust
// In crates/hof-core/src/ytdlp.rs
if platform == "youtube" && sponsorblock_enabled {
    extractor.with_arg("--sponsorblock-mark=all".to_string());
}
```

Parse chapters from yt-dlp response - SponsorBlock segments appear as chapters with `[SponsorBlock]` prefix.

### Phase 3: Segment Storage

```sql
-- Store SponsorBlock segments per video
CREATE TABLE video_sponsorblock_segments (
    id TEXT PRIMARY KEY,
    video_id TEXT NOT NULL REFERENCES videos(id) ON DELETE CASCADE,
    category TEXT NOT NULL,           -- 'sponsor', 'intro', etc.
    start_time REAL NOT NULL,         -- seconds
    end_time REAL NOT NULL,           -- seconds
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_sponsorblock_video ON video_sponsorblock_segments(video_id);
```

### Phase 4: Post-Processing

After download completes, if `sponsorblock_mode = 'remove'`:

```rust
// Use ffmpeg to remove segments
// Build filter_complex with segment times
let filter = segments
    .iter()
    .map(|s| format!("between(t,{},{})", s.start_time, s.end_time))
    .collect::<Vec<_>>()
    .join("+");

// ffmpeg -i input.mp4 -vf "select='not(between(t,0,30)+between(t,120,150))'" -af "aselect='...'" output.mp4
```

Alternative: Use ffmpeg's concat demuxer for lossless segment removal (more complex but no re-encode).

### Phase 5: Configuration

```bash
# Environment variables (global defaults)
SPONSORBLOCK_DEFAULT_MODE=disabled       # disabled, mark, remove
SPONSORBLOCK_DEFAULT_CATEGORIES=sponsor  # comma-separated

# Per-profile override via API/UI
```

### Phase 6: UI Updates

- Add SponsorBlock settings to profile edit form
- Show segment count on video details page
- Display chapters in video player (if mark mode)

---

## Nix/Container Changes

No changes required to `flake.nix` for container builds - yt-dlp binary already includes SponsorBlock support. The `ffmpeg-headless` package already in the container is sufficient for post-processing.

For development shell, no additional packages needed.

---

## Testing Strategy

### Unit Tests
- Segment parsing from yt-dlp chapter metadata
- ffmpeg filter string generation
- Category filtering logic

### Integration Tests
- Mock yt-dlp output with SponsorBlock chapters
- Verify segment storage/retrieval
- Test mode switching (disabled/mark/remove)

### Manual Testing
- Test with real YouTube video containing sponsors
- Verify segment removal accuracy
- Check A/V sync after processing

---

## Future Enhancements

1. **Local segment submission**: Allow users to submit segments to SponsorBlock API
2. **Segment preview**: Show segments before removal for user confirmation
3. **Skip-only mode**: For streaming/playback, skip segments without re-encoding
4. **Caching**: Cache SponsorBlock API responses to reduce requests

---

## Dependencies

| Crate/Tool | Purpose | Already Available |
|------------|---------|-------------------|
| `yt-dlp` | Segment data via `--sponsorblock-mark` | Yes (CLI) |
| `ffmpeg` | Segment removal post-processing | Yes (container) |

No new Rust crate dependencies required for Option 1.

---

## Implementation Order

1. [ ] Database migration for profile settings + segment storage
2. [ ] Modify metadata extraction to fetch SponsorBlock data
3. [ ] Parse and store segments from yt-dlp output
4. [ ] Implement ffmpeg post-processing for segment removal
5. [ ] Add profile settings UI
6. [ ] Add environment variable configuration
7. [ ] Tests

---

## References

- [SponsorBlock API](https://wiki.sponsor.ajay.app/w/API_Docs)
- [yt-dlp SponsorBlock options](https://github.com/yt-dlp/yt-dlp#sponsorblock-options)
- [ffmpeg select filter](https://ffmpeg.org/ffmpeg-filters.html#select_002c-aselect)
