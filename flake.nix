{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
    }:
    let
      # Systems we want to be able to build ON (e.g. your laptop)
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSys = nixpkgs.lib.genAttrs supportedSystems;

      # Helper to create a package definition so we don't repeat ourselves
      makeUbcPackage =
        pkgs:
        pkgs.rustPlatform.buildRustPackage {
          pname = "ubc125";
          version = "0.2.0";
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = [
            pkgs.pkg-config
            pkgs.protobuf
          ];
          # System libraries linked into the binary (alsa + opus): buildInputs
          # so buildRustPackage feeds their pkgconfig dirs to PKG_CONFIG_PATH
          # and they end up in the runtime closure.
          buildInputs = [ pkgs.alsa-lib pkgs.libopus ];
        };
    in
    {
      packages = forAllSys (
        system:
        let
          # Standard native packages
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };

          # Cross-compilation pkgs: Host is 'system' (x86), Target is 'aarch64'
          pkgsCross = import nixpkgs {
            inherit system;
            # crossSystem is a special configuration parameter passed to the nixpkgs import
            crossSystem = "aarch64-linux";
            overlays = [ rust-overlay.overlays.default ];
          };
        in
        {
          # 1. Native Build (runs 'nix build .#ubc125')
          ubc125 = makeUbcPackage pkgs;

          # 2. Cross Build (runs 'nix build .#ubc125-aarch64')
          ubc125-aarch64 = makeUbcPackage pkgsCross;

          default = self.packages.${system}.ubc125;
        }
      );

      devShells = forAllSys (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };
          toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        in
        {
          default = pkgs.mkShell {
            packages = [
              toolchain
              pkgs.pkg-config
              pkgs.protobuf
              pkgs.alsa-lib
              pkgs.libopus
              pkgs.rust-analyzer-unwrapped
            ];
            RUST_SRC_PATH = "${toolchain}/lib/rustlib/src/rust/library";
          };
        }
      );

      # NixOS module: run `ubc125 serve` as a systemd service on the Pi.
      nixosModules.default =
        {
          lib,
          config,
          pkgs,
          ...
        }:
        let
          cfg = config.services.ubc125;
        in
        {
          options.services.ubc125 = {
            enable = lib.mkEnableOption "the UBC125XLT scanner serve daemon (gRPC/gRPC-Web server)";

            package = lib.mkOption {
              type = lib.types.package;
              default = self.packages.${pkgs.stdenv.hostPlatform.system}.ubc125;
              description = "The ubc125 package to run";
            };

            listenAddress = lib.mkOption {
              type = lib.types.str;
              default = "0.0.0.0:50051";
              description = "Address:port the gRPC/gRPC-Web server listens on";
            };

            device = lib.mkOption {
              type = lib.types.str;
              default = "";
              description = "Scanner serial device. An empty string auto-detects the port from the scanner's USB id (1965:0018).";
            };

            audioDevice = lib.mkOption {
              type = lib.types.str;
              default = "";
              description = "ALSA capture device for the audio pipeline. An empty string uses the built-in default (the Pi's USB mic, card 2).";
            };

            declick = lib.mkOption {
              type = lib.types.bool;
              default = false;
              description = "Enable the experimental squelch de-clicker on the audio pipeline (a 1000 ms floor-anchored close fade and a 20 ms onset fade).";
            };

            audioClusterMs = lib.mkOption {
              type = lib.types.int;
              default = 60;
              description = "WebM cluster duration in milliseconds for the audio pipeline (cluster/20 Opus frames per cluster). Lower means less buffering latency; accepted range 20..=1000.";
            };

            audioSubscriberQueue = lib.mkOption {
              type = lib.types.int;
              default = 8;
              description = "Per-Listen-subscriber bounded queue depth in chunks; drop-oldest when full. Accepted range 1..=256.";
            };
          };

          config = lib.mkIf cfg.enable {
            systemd.services.ubc125-serve = {
              description = "UBC125XLT radio scanner gRPC server";
              wantedBy = [ "multi-user.target" ];
              after = [ "network.target" ];

              serviceConfig = {
                DynamicUser = true;
                # dialout: open the scanner's ttyACM* port; audio: ALSA /
                # dev/snd capture for the Listen stream. A static user would
                # need `users.users.<name>.extraGroups = [ \"dialout\" \"audio\" ];`
                # instead.
                SupplementaryGroups = [ "dialout" "audio" ];
                ExecStart = "${cfg.package}/bin/ubc125 serve --server-addr ${cfg.listenAddress}"
                  + lib.optionalString (cfg.device != "") " --device ${cfg.device}"
                  + lib.optionalString (cfg.audioDevice != "") " --audio-device ${cfg.audioDevice}"
                  + lib.optionalString cfg.declick " --declick"
                  + " --audio-cluster-ms ${toString cfg.audioClusterMs}"
                  + " --audio-subscriber-queue ${toString cfg.audioSubscriberQueue}";
                # If the scanner is not (yet) connected, serve exits with an
                # error and systemd retries every 10 s until it appears.
                Restart = "on-failure";
                RestartSec = "10s";
              };
            };
          };
        };
    };
}
