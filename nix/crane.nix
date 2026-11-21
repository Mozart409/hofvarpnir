# The crane inputs shared by the flake's package/container and nix/checks.nix:
# the source filter, the common build arguments and the dependency build.
#
#   import ./nix/crane.nix {inherit pkgs crane toolchain;}
{
  pkgs,
  crane,
  toolchain,
}: let
  # Crane for building Rust packages
  craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;

  # Common source filtering for Rust builds.
  # Include SQLx migration files so `sqlx::migrate!("./migrations")`
  # embeds the full migration set in container builds.
  # Include hof-web assets (app.css, etc.) for static file serving.
  # Include .sqlx/ directory so SQLX_OFFLINE=true works (query metadata).
  src = pkgs.lib.cleanSourceWith {
    src = ../.;
    filter = path: type:
      (craneLib.filterCargoSources path type)
      || pkgs.lib.hasInfix "/crates/hof-core/migrations/" (toString path)
      || pkgs.lib.hasInfix "/crates/hof-web/assets/" (toString path)
      || pkgs.lib.hasInfix "/.sqlx/" (toString path);
  };

  # Common build arguments
  commonArgs = {
    inherit src;
    pname = "hofvarpnir";
    strictDeps = true;

    # Build dependencies (native)
    nativeBuildInputs = with pkgs; [
      pkg-config
    ];

    # Runtime dependencies
    buildInputs = with pkgs; [
      openssl
    ];

    # Environment variables for SQLx offline mode
    SQLX_OFFLINE = "true";
  };

  # Build dependencies separately for caching
  cargoArtifacts = craneLib.buildDepsOnly commonArgs;
in {
  inherit craneLib src commonArgs cargoArtifacts;
}
