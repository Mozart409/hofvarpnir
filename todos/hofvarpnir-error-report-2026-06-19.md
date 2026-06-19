# Hofvarpnir Error Report
**Date:** 2026-06-19
**System:** Hofvarpnir (`hofvarpnir.dropbear-butterfly.ts.net`)

---

## 1. Summary

The video archival system **Hofvarpnir** has a critical indexing failure on the `PietSmiet GTA Online` playlist. The indexer has been silently failing since **May 2026** due to age-restricted videos in the playlist. This causes the entire scan to abort, meaning new uploads are never detected. Additionally, the system-wide `yt-dlp` version is outdated (90+ days), increasing YouTube blocking/rate-limit risk.

---

## 2. Affected Source

| Field | Value |
|---|---|
| **Source Name** | PietSmiet GTA Online |
| **Source ID** | `01KMNW685SYDZG7SCRAMTV3DT9` |
| **Profile ID** | `01KM3XM25NNCV1EZ68QX7EZF9R` |
| **Type** | Playlist |
| **URL** | `https://www.youtube.com/playlist?list=PL5JK9SjdCJp_yHv3fv-MlBt1AhGJnQ1iW` |
| **Status** | Enabled |
| **Index Rate** | 3 days (changed from 1 day on 2026-06-19) |
| **Cutoff Date** | `2026-03-26` |
| **Last Indexed** | `2026-06-18T23:29:28Z` (errored) |
| **Last Success** | No `SourceIndexed` success events since at least May 2026 |

---

## 3. Root Cause

### 3.1 Age-Restricted Videos Blocking Playlist Scan
The playlist contains two age-restricted video IDs that `yt-dlp` cannot process without authentication cookies:

- `wLxUgGoErSU`
- `rZyUDmT_Ch8`

When `yt-dlp` encounters these videos, it exits with code 1 and the error:
```
ERROR: [youtube] <video_id>: Sign in to confirm your age. 
This video may be inappropriate for some users. 
Use --cookies-from-browser or --cookies for the authentication.
```

Hofvarpnir's indexer does not handle this gracefully — the entire playlist scan aborts, so any videos **after** the blocked one in the playlist order are never discovered.

### 3.2 yt-dlp Outdated
The system is running `yt-dlp` version **2026.03.17** (Nix store path: `/nix/store/x892vvnznz4vvpyyqarrfygrjlfld43m-yt-dlp-2026.03.17`).

yt-dlp itself warns:
```
WARNING: Your yt-dlp version (2026.03.17) is older than 90 days!
It is strongly recommended to always use the latest version.
```

An outdated version increases the risk of YouTube rate-limiting (HTTP 429), extractor bugs, and failing to handle newer YouTube anti-bot measures.

### 3.3 First Occurrence
The earliest recorded error in the activity log is **2026-05-03**, and the pattern has repeated **every single day** since then (86 total errors on this source). This means the playlist has been completely broken for **~6-7 weeks**.

---

## 4. Impact

| Impact | Detail |
|---|---|
| **New uploads missed** | Any video added to the playlist after the blocked video(s) is never detected. The user reported a video from 2 days ago was missed. |
| **Silent failure** | The source is marked as `Enabled` and `last_indexed_at` is updated, but the activity log shows `SourceError` — not `SourceIndexed`. No alerting is visible. |
| **Storage bloat** | One existing video was successfully downloaded (`Die BESTE FLUCHT vor den HITMEN!`, 3.9 GB) but has already been **cleaned** (status: `Cleaned`). |
| **Rate limit risk** | Repeated daily failures with an outdated yt-dlp may contribute to YouTube IP/rate-limiting. |

---

## 5. Recommended Fixes

### 5.1 Immediate Fix: Update yt-dlp
Update the NixOS configuration to pull a newer `yt-dlp` version. In the Nix flake or configuration:
- Run `nix flake update` or bump the `nixpkgs` input.
- Ensure the `yt-dlp` derivation in the Hofvarpnir service is updated.

### 5.2 Critical Fix: Skip Age-Restricted Videos
Modify the yt-dlp invocation in the Hofvarpnir indexer to add:
```
--ignore-errors
--no-abort-on-error
```

This ensures that if a single video in the playlist is age-restricted, unavailable, or private, yt-dlp logs the error and continues scanning the rest of the playlist.

### 5.3 Optional Fix: Provide YouTube Cookies
If the age-restricted videos **should** be archived, provide a cookies file:
1. Export cookies from a browser with a logged-in YouTube account.
2. Pass the cookies via `--cookies /path/to/cookies.txt` in the yt-dlp invocation.

**Note:** Cookies will age and require periodic renewal. `--ignore-errors` is the safer, maintenance-free approach for playlists with mixed content.

### 5.4 Monitoring Fix: Alert on `SourceError`
Currently, the only way to detect this is by checking the activity log. Consider adding:
- A Prometheus alert or health-check endpoint that flags sources with >N consecutive `SourceError` events.
- A dashboard or CLI report that lists "enabled but erroring" sources.

### 5.5 Cutoff Date Review
The cutoff date for this source is `2026-03-26`. If the playlist is meant to backfill older content, this is fine. If it should catch recent videos, the cutoff is correct (today is 2026-06-19).

---

## 6. Appendix: Other Sources Checked

During the investigation, I updated 72 inactive channels from 1-day to 3-day index frequency. The 7 active channels (with new uploads in last indexing) were left at their original rate. No other sources showed the same persistent `SourceError` pattern, but the `yt-dlp` version issue affects all sources globally.

---

## 7. Files / References

- **API Docs:** `https://hofvarpnir.dropbear-butterfly.ts.net/docs/openapi.json`
- **Source Activity:** `GET /api/v1/activity?source_id=01KMNW685SYDZG7SCRAMTV3DT9`
- **Source Details:** `GET /api/v1/sources/01KMNW685SYDZG7SCRAMTV3DT9`
- **yt-dlp Wiki:** https://github.com/yt-dlp/yt-dlp/wiki/FAQ#how-do-i-pass-cookies-to-yt-dlp
