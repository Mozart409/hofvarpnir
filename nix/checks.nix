# What `just ci-gate` runs (fmt, clippy, the tests including the Postgres ones,
# and `just test-patches` for the vendored yt-dlp fork) as crane checks, called
# from the root flake; `cargo deny` and `cargo audit` stay plain recipes (they
# fetch the advisory database, and a build has no network):
#
#   import ./rust/hofvarpnir/nix/checks.nix {inherit pkgs crane toolchain testPostgres;}
#
# As Nix builds they are cached by their inputs: a push that does not change
# the source finds them already in the store, and the pre-push rust-gate
# (the root's legacyPackages.rust-gate) costs an evaluation instead of
# a compile. The crane inputs are the package's (./crane.nix).
#
# Returns `checks` and `cargoArtifacts`.
{
  pkgs,
  crane,
  toolchain,
  # rust/test-postgres.nix
  testPostgres,
}: let
  inherit (pkgs) lib;

  hof = import ./crane.nix {inherit pkgs crane toolchain;};
  inherit (hof) craneLib;

  version = (lib.importTOML ../Cargo.toml).workspace.package.version;

  # The package's source, plus the test fixtures (an `include_str!` in the unit
  # tests, the TLS material of otel_export) that `--all-targets` and the tests
  # need and the package build does not.
  src = lib.cleanSourceWith {
    src = ../.;
    filter = path: type:
      hof.src.filter path type
      || lib.hasInfix "/tests/fixtures/" (toString path);
  };

  commonArgs =
    hof.commonArgs
    // {
      inherit src version;
      # The dev profile `cargo test` uses, so the tests keep debug assertions
      # and overflow checks (crane defaults to release).
      CARGO_PROFILE = "dev";
    };

  cargoArtifacts = craneLib.buildDepsOnly commonArgs;

  # The vendored yt-dlp fork is its own workspace (own Cargo.lock), wired into
  # the main one through [patch.crates-io], so the main `cargo test` compiles
  # it but never tests it. Its own source and dependency build.
  patchesSrc = lib.cleanSource ../patches/yt-dlp-patched;

  patchesArgs = {
    src = patchesSrc;
    pname = "yt-dlp-patched";
    version = (lib.importTOML ../patches/yt-dlp-patched/Cargo.toml).package.version;
    inherit (hof.commonArgs) strictDeps nativeBuildInputs buildInputs;
    CARGO_PROFILE = "dev";
  };

  patchesArtifacts = craneLib.buildDepsOnly patchesArgs;
in {
  inherit cargoArtifacts;

  checks = {
    fmt = craneLib.cargoFmt {
      inherit src version;
    };

    clippy = craneLib.cargoClippy (commonArgs
      // {
        inherit cargoArtifacts;
        # Kept equal to `just ci-gate`.
        cargoClippyExtraArgs = "--workspace --all-targets --all-features -- -D warnings";
      });

    # `just ci-gate`'s test step: the #[sqlx::test]s against a Postgres that
    # lives for the run, `--include-ignored` as there.
    test = craneLib.cargoTest (testPostgres.withPostgres "DATABASE_URL" (commonArgs
      // {
        inherit cargoArtifacts;
        cargoTestExtraArgs = "--all-features -- --include-ignored";
        # hof-core's verify tests and the e2e download-pipeline tests run
        # `ffmpeg`/`ffprobe` (generated clips, lavfi, mpeg4/aac), and the
        # pipeline tests run a real `yt-dlp` against a local wiremock server.
        nativeCheckInputs = [pkgs.ffmpeg-headless pkgs.yt-dlp];
        # reqwest's rustls verifier loads the system roots when a client is
        # built, and the sandbox has none.
        SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
        # yt-dlp wants a writable cache and config home.
        preCheck = ''
          export HOME="$TMPDIR/home" XDG_CACHE_HOME="$TMPDIR/cache"
          mkdir -p "$HOME" "$XDG_CACHE_HOME"
        '';
      }));

    # `just test-patches`: the offline targets only (upstream's `integration`
    # and `e2e` suites hit the network), then the doctests.
    patches-test = craneLib.cargoTest (patchesArgs
      // {
        cargoArtifacts = patchesArtifacts;
        checkPhaseCargoCommand = ''
          cargoWithProfile test --lib --test unit
          cargoWithProfile test --doc
        '';
      });
  };
}
