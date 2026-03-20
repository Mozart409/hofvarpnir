{
  description = "Hofvarpnir - Video archival system";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
    crane.url = "github:ipetkov/crane";
  };

  outputs = {
    self,
    nixpkgs,
    flake-utils,
    rust-overlay,
    crane,
  }:
    flake-utils.lib.eachSystem ["x86_64-linux" "aarch64-linux"] (system: let
      pkgs = import nixpkgs {
        inherit system;
        config.allowUnfree = true;
        overlays = [rust-overlay.overlays.default];
      };

      ociImageVersion = builtins.getEnv "OCI_IMAGE_VERSION";
      ociImageRevision = builtins.getEnv "OCI_IMAGE_REVISION";
      ociImageCreated = builtins.getEnv "OCI_IMAGE_CREATED";

      rust = pkgs.rust-bin.nightly."2026-02-15".default.override {
        extensions = ["rustfmt" "clippy" "rust-src"];
      };

      # Crane for building Rust packages
      craneLib = (crane.mkLib pkgs).overrideToolchain rust;

      # Common source filtering for Rust builds.
      # Include SQLx migration files so `sqlx::migrate!("./migrations")`
      # embeds the full migration set in container builds.
      # Include hof-web assets (app.css, etc.) for static file serving.
      src = pkgs.lib.cleanSourceWith {
        src = ./.;
        filter = path: type:
          (craneLib.filterCargoSources path type)
          || pkgs.lib.hasInfix "/crates/hof-core/migrations/" (toString path)
          || pkgs.lib.hasInfix "/crates/hof-web/assets/" (toString path);
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
      hofvarpnir = craneLib.buildPackage (commonArgs // {
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

      imageVersion = if ociImageVersion != "" then ociImageVersion else "dev-${self.shortRev or "unknown"}";
      imageRevision = if ociImageRevision != "" then ociImageRevision else self.rev or self.dirtyRev or "unknown";
      imageCreated = let
        defaultTs = self.lastModifiedDate or "19700101000000";
        defaultCreated = "${builtins.substring 0 4 defaultTs}-${builtins.substring 4 2 defaultTs}-${builtins.substring 6 2 defaultTs}T${builtins.substring 8 2 defaultTs}:${builtins.substring 10 2 defaultTs}:${builtins.substring 12 2 defaultTs}Z";
      in if ociImageCreated != "" then ociImageCreated else defaultCreated;

      commonLabels = {
        "org.opencontainers.image.title" = "hofvarpnir";
        "org.opencontainers.image.description" = "Video archival system with yt-dlp";
        "org.opencontainers.image.url" = "https://github.com/Mozart409/hofvarpnir";
        "org.opencontainers.image.source" = "https://github.com/Mozart409/hofvarpnir";
        "org.opencontainers.image.documentation" = "https://github.com/Mozart409/hofvarpnir#readme";
        "org.opencontainers.image.licenses" = "SEE LICENSES";
        "org.opencontainers.image.authors" = "Mozart409";
        "org.opencontainers.image.vendor" = "Mozart409";
        "org.opencontainers.image.version" = imageVersion;
        "org.opencontainers.image.revision" = imageRevision;
        "org.opencontainers.image.created" = imageCreated;
      };
    in {
      # Rust package
      packages = {
        default = hofvarpnir;
        hofvarpnir = hofvarpnir;

        # OCI container image
        # Build: nix build .#container
        # Load:  docker load < result
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
            pkgs.ffmpeg

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
              Timeout = 10 * 1000000000;  # 10 seconds
              Retries = 3;
              StartPeriod = 60 * 1000000000; # 60 seconds
            };
          };
        };

        # Minimal container without debugging tools
        container-minimal = pkgs.dockerTools.buildLayeredImage {
          name = "hofvarpnir";
          tag = "minimal";

          contents = [
            hofvarpnir
            licenseBundle
            pkgs.yt-dlp
            pkgs.ffmpeg
            pkgs.cacert
            # wget for healthcheck only
            pkgs.wget
          ];

          # Enable fakeroot for chown support
          enableFakechroot = true;
          fakeRootCommands = ''
            mkdir -p ./etc
            echo "${containerUser}:x:${containerUid}:${containerGid}::/home/${containerUser}:/bin/false" > ./etc/passwd
            echo "${containerUser}:x:${containerGid}:" > ./etc/group
            mkdir -p ./home/${containerUser}
            chown ${containerUid}:${containerGid} ./home/${containerUser}
            mkdir -p ./data/downloads ./data/incomplete ./tmp
            chown ${containerUid}:${containerGid} ./data/downloads ./data/incomplete
            chmod 1777 ./tmp
          '';

          config = {
            Cmd = ["/bin/hofvarpnir"];
            User = "${containerUid}:${containerGid}";
            ExposedPorts."3000/tcp" = {};
            Env = [
              "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
              "HOME=/home/${containerUser}"
              "PORT=3000"
              "RUST_LOG=info"
              "YTDLP_PATH=${pkgs.yt-dlp}/bin/yt-dlp"
              "DEFAULT_OUTPUT_DIR=/data/downloads"
            ];
            Volumes = {
              "/data/downloads" = {};
              "/data/incomplete" = {};
            };
            WorkingDir = "/home/${containerUser}";
            Labels = commonLabels;
            Healthcheck = {
              Test = ["CMD" "wget" "-q" "--spider" "http://localhost:3000/api/health/ready"];
              Interval = 30 * 1000000000;
              Timeout = 10 * 1000000000;
              Retries = 3;
              StartPeriod = 60 * 1000000000;
            };
          };
        };
      };

      # to use other shells, run:
      # nix develop . --command fish
      devShells.default = pkgs.mkShell {
        buildInputs = with pkgs; [
          rust
          cargo-workspaces
          opentofu
          sqlx-cli
          actionlint
          bacon
          cargo-audit
          cargo-deny
          cargo-outdated
          cargo-workspaces
          cocogitto
          dbeaver-bin
          docker
          docker-buildx
          docker-compose
          just
          keep-sorted
          lazydocker
          lefthook
          nodejs_24
          opencode
          opentofu
          dbeaver-bin
          playwright-driver.browsers
          postgresql_17
          rust
          sqlx-cli
          sqruff
          tailwindcss_4
          trivy
          ffmpeg
          yt-dlp
          # Container image tools
          skopeo  # For pushing OCI images to registries
        ];
        shellHook = ''
          lefthook install
          cog install-hook
          yt-dlp --version
          export COMPOSE_BAKE=true
          export PLAYWRIGHT_BROWSERS_PATH=${pkgs.playwright-driver.browsers}
          export PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS=true
        '';
      };
    });
}
