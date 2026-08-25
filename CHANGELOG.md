# Changelog
All notable changes to this project will be documented in this file. See [conventional commits](https://www.conventionalcommits.org/) for commit guidelines.

- - -
## v0.7.0 - 2026-08-25
#### Features
- (**test**) add dedicated lean postgres-test service for test suite - (07b3965) - Amadeus Mader
#### Bug Fixes
- (**ytdlp**) stop codec preference capping resolution below profile quality - (df442ef) - Amadeus Mader
#### Miscellaneous Chores
- (**css**) update css build - (3b740a3) - Amadeus Mader

- - -

## v0.6.2 - 2026-08-25
#### Bug Fixes
- (**actors**) stop periodic tickers dying on transient mailbox-full - (59a3c69) - Amadeus Mader
- (**cleanup**) treat unset retention as keep-forever, not expired - (92c53db) - macbook-pro
- (**tools**) fix justfile - (d3b9aa1) - Amadeus Mader
- (**tools**) check also if container is running - (8c93d51) - macbook-pro
- (**tools**) fix justfile - (108d1b0) - Amadeus Mader
#### Documentation
- (**yt-dlp**) add indexing timeout report - (f4eb3ce) - developmentbot
- (**yt-dlp**) add indexing timeout report - (8cec083) - developmentbot
#### Miscellaneous Chores
- (**ci**) bump github/codeql-action/analyze from 4.37.4 to 4.37.7 - (5bb7cf3) - dependabot[bot]
- (**cleanup**) default cleanup interval to 3h instead of 15m - (08dff0f) - macbook-pro
- (**deps**) upgrade deps - (9d13ace) - macbook-pro
- (**flake**) pin yt-dlp to 2026.08.19 - (4bc5fc3) - developmentbot
- (**flake**) update inputs - (4bebd5b) - developmentbot
- (**flake**) pin yt-dlp to 2026.08.19 - (16db39e) - developmentbot
- (**just**) ignore unreachable deno CVEs in trivy scan - (29722b0) - developmentbot
- (**just**) ignore unreachable deno CVEs in trivy scan - (edb0883) - developmentbot
- (**just**) drop flake update from trivy recipe - (7f5adc2) - developmentbot
- (**tools**) increase threads - (345e021) - macbook-pro
- (**tools**) add darwin toolchain - (e83365d) - macbook-pro
- (**tools**) add sync-remotes script - (83ec3b0) - macbook-pro
- (**tools**) add short circuits - (d92ab9c) - developmentbot
- (**tools**) increase threads - (6bb2113) - macbook-pro
- (**tools**) add short circuits - (2c0d29d) - developmentbot
- (**version**) v0.6.1 - (4201d63) - macbook-pro
- (**version**) v0.6.0 - (58ecb27) - Amadeus Mader

- - -

## v0.6.1 - 2026-08-22
#### Bug Fixes
- (**cleanup**) treat unset retention as keep-forever, not expired - (3f1eedd) - macbook-pro
- (**tools**) check also if container is running - (2d4cd74) - macbook-pro
#### Miscellaneous Chores
- (**cleanup**) default cleanup interval to 3h instead of 15m - (21b805a) - macbook-pro
- (**deps**) upgrade deps - (69caeb0) - macbook-pro
- (**just**) skip compose up if postgres already healthy - (cc2f5fc) - macbook-pro
- (**tools**) add darwin toolchain - (ddc6573) - macbook-pro
- (**tools**) add sync-remotes script - (7d1428f) - macbook-pro

- - -

## v0.6.0 - 2026-08-07
#### Features
- (**activity**) add message search and source filter pill - (0ed7501) - developmentbot
- (**downloads**) add bulk retry, cancel and delete - (3d626e9) - developmentbot
- (**sources**) add exclude from cleanup toggle - (ca4af29) - developmentbot
- (**web**) add search to sources and schedule pages - (858e680) - developmentbot
#### Bug Fixes
- (**web**) push navigable urls and stop partials returning full documents - (b250c74) - developmentbot
#### Tests
- (**activity**) assert api severity casing and add e2e-only recipe - (c601d5a) - developmentbot
- cover search, bulk actions and cleanup exclusion - (91677d8) - developmentbot
#### Miscellaneous Chores
- (**deps**) upgrade flake and rust to 1.97.1 - (915a381) - Amadeus Mader
- (**tools**) scope pre-push sqruff lint to pushed files - (74f6019) - developmentbot
- (**tools**) add attic cache - (d402f8f) - Amadeus Mader

- - -

## v0.5.1 - 2026-07-31
#### Bug Fixes
- (**web**) wrap long error messages in activity - (e5ab1e1) - Amadeus Mader
#### Miscellaneous Chores
- (**css**) update css - (79fe073) - Amadeus Mader
- (**tools**) add sqlx-prepare - (9331f1f) - Amadeus Mader

- - -

## v0.5.0 - 2026-07-30
#### Continuous Integration
- (**flakehub**) remove flakehub stuff - (43ae4d0) - Amadeus Mader
#### Miscellaneous Chores
- (**deps**) bump quinn-proto in /patches/yt-dlp-patched - (fc38154) - dependabot[bot]

- - -

## v0.4.0 - 2026-07-30
#### Features
- (**api**) add source and profile context to downloads response - (4f2b656) - Amadeus Mader
- (**dashboard**) show storage quota usage card - (fc7d96f) - Amadeus Mader
- (**downloads**) remove live progress section - (f085264) - Amadeus Mader
#### Bug Fixes
- (**activity**) show source pill on download events - (6e5371d) - Amadeus Mader
- (**api**) assert actual json casing for download status in test - (483a5f5) - Amadeus Mader
- (**api**) redirect /docs/ to /docs - (257ead2) - Amadeus Mader
- (**indexer**) tolerate null video titles in playlist json - (483d213) - Amadeus Mader
- (**web**) use decimal GB for storage quota display - (e0b7f88) - Amadeus Mader
#### Documentation
- (**ytdlp**) record vendored patch delta and re-sync steps - (2ade9cc) - Amadeus Mader
#### Continuous Integration
- (**codeql**) fix version mismatch - (c7cb64a) - Amadeus Mader
#### Refactoring
- (**ulid**) upgrade to ulid v3 - (aba8c3b) - Amadeus Mader
#### Miscellaneous Chores
- (**ci**) type-check vendored patch crates - (35187ff) - Amadeus Mader
- (**css**) minifiy - (6a83af8) - Amadeus Mader
- (**deps**) upgrade flake - (0b72637) - Amadeus Mader
- (**tools**) remove cog install hook as its covered by lefthook - (fb15908) - Amadeus Mader

- - -

## v0.3.1 - 2026-07-23
#### Bug Fixes
- (**downloads**) dispatch pending downloads inline to avoid mailbox drops - (94bbfbf) - Amadeus Mader
- (**indexer**) target /videos tab for bare YouTube channel URLs - (0758401) - Amadeus Mader

- - -

## v0.3.0 - 2026-07-23
#### Features
- (**activity**) show source name in log entries - (507bb90) - Amadeus Mader
- (**api-keys**) add 1h/1d/7d expiration presets - (15d94b5) - Amadeus Mader
- (**schedule**) sink disabled sources with disabled pill - (499956d) - Amadeus Mader
#### Bug Fixes
- (**downloads**) dispatch pending downloads inline to avoid mailbox drops - (1de8672) - Amadeus Mader
- (**indexer**) target /videos tab for bare YouTube channel URLs - (b45f7c1) - Amadeus Mader
#### Documentation
- (**agent**) update docs for commit conventions - (be01a0b) - Amadeus Mader
#### Miscellaneous Chores
- (**css**) update tailwindcss - (1d5466c) - Amadeus Mader
- (**deps**) upgrade flake - (ed7a599) - Amadeus Mader
- (**deps**) bump serde_with in /patches/yt-dlp-patched - (9085672) - dependabot[bot]

- - -

## v0.2.5 - 2026-07-21
#### Bug Fixes
- (**release**) run pre-bump cargo check with SQLX_OFFLINE - (a2d42da) - Amadeus Mader
- (**release**) scope cargo set-version to hof-web to skip patch crates - (e7ea8d5) - Amadeus Mader
#### Miscellaneous Chores
- (**deps**) upgrade ulid to v2 - (e079da0) - Amadeus Mader
- (**deps**) upgrade to rust 1.96.1 - (d63b4b0) - Amadeus Mader
- (**harbor**) push to harbor - (e2898ff) - Amadeus Mader
- (**oci**) add versioned Harbor release script and speed up push-oci - (a8afe2e) - Amadeus Mader
- (**release**) drop release.sh, move guards into cog.toml - (11c94c0) - Amadeus Mader

- - -

## v0.2.4 - 2026-07-10
#### Bug Fixes
- (**ci**) change cleanup workflow and add verification to build step - (52bba33) - Amadeus Mader
- (**tools**) update cachix cmds - (8a7b8b1) - Amadeus Mader
#### Miscellaneous Chores
- (**ci**) pin to tag - (8bc9a87) - Amadeus Mader
- (**deps**) upgrade flake - (9c19203) - Amadeus Mader
- (**flake**) bump Rust toolchain to 1.96.0 - (4ad92d4) - Amadeus Mader
- (**tools**) update release script - (31b138d) - Amadeus Mader

- - -

Changelog generated by [cocogitto](https://github.com/cocogitto/cocogitto).