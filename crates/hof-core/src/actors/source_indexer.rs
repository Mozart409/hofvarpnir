//! The `SourceIndexerActor` is spawned per source. It calls
//! `yt-dlp --flat-playlist --dump-json` to discover new videos, filters them
//! by the source's cutoff date and profile settings (shorts, livestreams),
//! then sends `EnqueueDownload` messages to the `DownloadSupervisor`.
