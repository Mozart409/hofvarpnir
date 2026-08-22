{
  description = "Hofvarpnir - Video archival system";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # nixos-unstable dropped x86_64-darwin support; use the last release
    # branch that still supports it (security fixes through end of 2026)
    # for that one system only, so Linux stays on nixos-unstable.
    nixpkgs-darwin.url = "github:NixOS/nixpkgs/nixpkgs-26.05-darwin";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
    crane.url = "github:ipetkov/crane";
  };

  outputs = {
    self,
    nixpkgs,
    nixpkgs-darwin,
    flake-utils,
    rust-overlay,
    crane,
  }:
    flake-utils.lib.eachSystem ["x86_64-linux" "aarch64-linux" "x86_64-darwin"] (system: let
      nixpkgsSource =
        if system == "x86_64-darwin"
        then nixpkgs-darwin
        else nixpkgs;
      pkgs = import nixpkgsSource {
        inherit system;
        config.allowUnfree = true;
        overlays = [
          rust-overlay.overlays.default
          # nixos-unstable lags upstream yt-dlp releases (which ship almost
          # daily to patch broken site extractors); pin to the latest
          # released version until nixpkgs catches up.
          (final: prev: {
            yt-dlp = prev.yt-dlp.overrideAttrs (_: {
              version = "2026.08.19";
              src = final.fetchFromGitHub {
                owner = "yt-dlp";
                repo = "yt-dlp";
                tag = "2026.08.19";
                hash = "sha256-BM5ZeGTmHq+1xH6G/zsuCtjLgYgfRA11ya0zIHK5p4g=";
              };
            });
          })
        ];
      };

      ociImageVersion = builtins.getEnv "OCI_IMAGE_VERSION";
      ociImageRevision = builtins.getEnv "OCI_IMAGE_REVISION";
      ociImageCreated = builtins.getEnv "OCI_IMAGE_CREATED";

      rust = pkgs.rust-bin.stable."1.97.1".default.override {
        extensions = ["rustfmt" "clippy" "rust-src"];
      };

      # Crane for building Rust packages
      craneLib = (crane.mkLib pkgs).overrideToolchain rust;

      # Common source filtering for Rust builds.
      # Include SQLx migration files so `sqlx::migrate!("./migrations")`
      # embeds the full migration set in container builds.
      # Include hof-web assets (app.css, etc.) for static file serving.
      # Include .sqlx/ directory so SQLX_OFFLINE=true works (query metadata).
      src = pkgs.lib.cleanSourceWith {
        src = ./.;
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

      # Build the hof-web binary
      hofvarpnir = craneLib.buildPackage (commonArgs
        // {
          inherit cargoArtifacts;

          # Only build the hof-web binary
          cargoExtraArgs = "-p hof-web";

          # Don't run tests during build (run separately)
          doCheck = false;
        });

      containerUser = "hofvarpnir";
      containerUid = "1000";
      containerGid = "1000";
      licenseBundle = pkgs.writeTextDir "licenses/THIRD_PARTY_LICENSES.md" (builtins.readFile ./THIRD_PARTY_LICENSES.md);

      # Pre-built binary path for CI (requires --impure)
      # Uses builtins.path to import the binary into the Nix store at eval time
      hofvarpnirBinaryPath = builtins.getEnv "HOFVARPNIR_BINARY";
      hofvarpnirBinaryStore =
        if hofvarpnirBinaryPath != ""
        then
          builtins.path {
            path = hofvarpnirBinaryPath;
            name = "hofvarpnir";
          }
        else throw "HOFVARPNIR_BINARY env var must be set when building containerFromBinary";
      hofvarpnirFromBinary = pkgs.runCommand "hofvarpnir-from-binary" {} ''
        mkdir -p $out/bin
        cp ${hofvarpnirBinaryStore} $out/bin/hofvarpnir
        chmod +x $out/bin/hofvarpnir
      '';

      imageVersion =
        if ociImageVersion != ""
        then ociImageVersion
        else "dev-${self.shortRev or "unknown"}";
      imageRevision =
        if ociImageRevision != ""
        then ociImageRevision
        else self.rev or self.dirtyRev or "unknown";
      imageCreated = let
        defaultTs = self.lastModifiedDate or "19700101000000";
        defaultCreated = "${builtins.substring 0 4 defaultTs}-${builtins.substring 4 2 defaultTs}-${builtins.substring 6 2 defaultTs}T${builtins.substring 8 2 defaultTs}:${builtins.substring 10 2 defaultTs}:${builtins.substring 12 2 defaultTs}Z";
      in
        if ociImageCreated != ""
        then ociImageCreated
        else defaultCreated;

      staticOciLabels = builtins.fromJSON (builtins.readFile ./oci-labels.json);
      commonLabels =
        staticOciLabels
        // {
          "org.opencontainers.image.version" = imageVersion;
          "org.opencontainers.image.revision" = imageRevision;
          "org.opencontainers.image.created" = imageCreated;
        };

      commonDevPackages = with pkgs; [
        # keep-sorted start

        actionlint
        bacon
        bun
        cargo-audit
        cargo-deny
        cargo-edit
        cargo-outdated
        cargo-shear
        cargo-watch
        cargo-workspaces
        # clang and cmake are build dependencies for aws-lc-sys,
        # which is the cryptography backend pulled in by rustls >= 0.23.40
        clang
        cmake
        cocogitto
        deadbranch
        ffmpeg
        git
        just
        keep-sorted
        lazydocker
        lefthook
        nodejs_24
        opentofu
        playwright-driver.browsers
        podman-compose
        podman-tui
        postgresql_17
        rust
        sqlx-cli
        sqruff
        tailwindcss_4
        yt-dlp
        # keep-sorted end
      ];

      linuxDevPackages = with pkgs; [
        dbeaver-bin
        podman
        podman-compose
        trivy
        opencode
        claude-code
        cachix
      ];

      darwinDevPackages = with pkgs; [
        actionlint
        podman
      ];
    in {
      # Rust package
      packages =
        {
          default = hofvarpnir;
          hofvarpnir = hofvarpnir;
        }
        // pkgs.lib.optionalAttrs (system == "x86_64-linux" || system == "aarch64-linux") ({
            # OCI container image (Linux only) - builds Rust via Crane
            # Build: nix build .#container
            # Load:  podman load < result
            # Push:  skopeo copy docker-archive:result docker://ghcr.io/user/hofvarpnir:tag
            container = pkgs.dockerTools.buildLayeredImage {
              name = "hofvarpnir";
              tag = "latest";

              contents = [
                # The application binary
                hofvarpnir

                # License bundle for mixed-license image contents
                licenseBundle

                # Required runtime dependencies
                pkgs.yt-dlp
                pkgs.ffmpeg-headless

                # TLS/SSL certificates for HTTPS connections
                pkgs.cacert

                # Minimal shell utilities for debugging (optional, remove for smaller image)
                pkgs.busybox
              ];

              # Enable fakeroot for chown support
              enableFakechroot = true;
              fakeRootCommands = ''
                # Create user and group
                mkdir -p ./etc
                echo "${containerUser}:x:${containerUid}:${containerGid}::/home/${containerUser}:/bin/sh" > ./etc/passwd
                echo "${containerUser}:x:${containerGid}:" > ./etc/group

                # Create home directory
                mkdir -p ./home/${containerUser}
                chown ${containerUid}:${containerGid} ./home/${containerUser}

                # Create downloads directory (will be mounted as volume)
                mkdir -p ./data/downloads
                chown ${containerUid}:${containerGid} ./data/downloads

                # Create incomplete downloads directory
                mkdir -p ./data/incomplete
                chown ${containerUid}:${containerGid} ./data/incomplete

                # Create tmp directory
                mkdir -p ./tmp
                chmod 1777 ./tmp
              '';

              config = {
                # Run as the application binary
                Cmd = ["/bin/hofvarpnir"];

                # Run as non-root user
                User = "${containerUid}:${containerGid}";

                # Expose the web server port
                ExposedPorts = {
                  "3000/tcp" = {};
                };

                # Environment variables
                Env = [
                  "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
                  "HOME=/home/${containerUser}"
                  "PORT=3000"
                  "RUST_LOG=info"
                  "YTDLP_PATH=${pkgs.yt-dlp}/bin/yt-dlp"
                  "DEFAULT_OUTPUT_DIR=/data/downloads"
                ];

                # Volume mount points for persistence
                Volumes = {
                  # Main downloads directory - mount this for Jellyfin/Syncthing access
                  "/data/downloads" = {};
                  # Incomplete downloads - can be ephemeral or persistent
                  "/data/incomplete" = {};
                };

                # Working directory
                WorkingDir = "/home/${containerUser}";

                # Labels for metadata
                Labels = commonLabels;

                # Health check - uses wget from busybox
                Healthcheck = {
                  Test = ["CMD" "wget" "-q" "--spider" "http://localhost:3000/api/health/ready"];
                  Interval = 30 * 1000000000; # 30 seconds in nanoseconds
                  Timeout = 10 * 1000000000; # 10 seconds
                  Retries = 3;
                  StartPeriod = 60 * 1000000000; # 60 seconds
                };
              };
            };
          }
          # Impure, CI-only image built from a pre-compiled binary. Only exposed
          # when HOFVARPNIR_BINARY is set so pure evaluation (flakehub-push,
          # `nix flake check`) never forces the `throw` in hofvarpnirBinaryStore.
          // pkgs.lib.optionalAttrs (hofvarpnirBinaryPath != "") {
            # OCI container from pre-built binary (for CI - no Rust compilation)
            # Build: HOFVARPNIR_BINARY=/path/to/binary nix build --impure .#containerFromBinary
            containerFromBinary = pkgs.dockerTools.buildLayeredImage {
              name = "hofvarpnir";
              tag = "latest";

              contents = [
                # Pre-built application binary (from CI artifacts)
                hofvarpnirFromBinary

                # License bundle for mixed-license image contents
                licenseBundle

                # Required runtime dependencies
                pkgs.yt-dlp
                pkgs.ffmpeg-headless

                # TLS/SSL certificates for HTTPS connections
                pkgs.cacert

                # Minimal shell utilities for debugging (optional, remove for smaller image)
                pkgs.busybox
              ];

              # Enable fakeroot for chown support
              enableFakechroot = true;
              fakeRootCommands = ''
                # Create user and group
                mkdir -p ./etc
                echo "${containerUser}:x:${containerUid}:${containerGid}::/home/${containerUser}:/bin/sh" > ./etc/passwd
                echo "${containerUser}:x:${containerGid}:" > ./etc/group

                # Create home directory
                mkdir -p ./home/${containerUser}
                chown ${containerUid}:${containerGid} ./home/${containerUser}

                # Create downloads directory (will be mounted as volume)
                mkdir -p ./data/downloads
                chown ${containerUid}:${containerGid} ./data/downloads

                # Create incomplete downloads directory
                mkdir -p ./data/incomplete
                chown ${containerUid}:${containerGid} ./data/incomplete

                # Create tmp directory
                mkdir -p ./tmp
                chmod 1777 ./tmp
              '';

              config = {
                # Run as the application binary
                Cmd = ["/bin/hofvarpnir"];

                # Run as non-root user
                User = "${containerUid}:${containerGid}";

                # Expose the web server port
                ExposedPorts = {
                  "3000/tcp" = {};
                };

                # Environment variables
                Env = [
                  "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
                  "HOME=/home/${containerUser}"
                  "PORT=3000"
                  "RUST_LOG=info"
                  "YTDLP_PATH=${pkgs.yt-dlp}/bin/yt-dlp"
                  "DEFAULT_OUTPUT_DIR=/data/downloads"
                ];

                # Volume mount points for persistence
                Volumes = {
                  # Main downloads directory - mount this for Jellyfin/Syncthing access
                  "/data/downloads" = {};
                  # Incomplete downloads - can be ephemeral or persistent
                  "/data/incomplete" = {};
                };

                # Working directory
                WorkingDir = "/home/${containerUser}";

                # Labels for metadata
                Labels = commonLabels;

                # Health check - uses wget from busybox
                Healthcheck = {
                  Test = ["CMD" "wget" "-q" "--spider" "http://localhost:3000/api/health/ready"];
                  Interval = 30 * 1000000000; # 30 seconds in nanoseconds
                  Timeout = 10 * 1000000000; # 10 seconds
                  Retries = 3;
                  StartPeriod = 60 * 1000000000; # 60 seconds
                };
              };
            };
          });

      # Minimal shell for CI builds (Rust + essentials only)
      devShells.ci = pkgs.mkShell {
        buildInputs = with pkgs; [
          rust
          cargo-edit
          pkg-config
          openssl
        ];
        SQLX_OFFLINE = "true";
      };

      # to use other shells, run:
      # nix develop . --command fish
      devShells.default = pkgs.mkShell {
        buildInputs =
          commonDevPackages
          ++ pkgs.lib.optionals pkgs.stdenv.isLinux linuxDevPackages
          ++ pkgs.lib.optionals pkgs.stdenv.isDarwin darwinDevPackages;
        shellHook = ''
          export COMPOSE_BAKE=true
          export PLAYWRIGHT_BROWSERS_PATH=${pkgs.playwright-driver.browsers}
          export PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS=true
          lefthook install
          yt-dlp --version
          du -sh ./target
          echo ""
        '';
      };
    });
}
