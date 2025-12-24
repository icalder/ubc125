{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
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
          # buildInputs = [ pkgs.udev ]; # Add if needed
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
              pkgs.rust-analyzer-unwrapped
            ];
            RUST_SRC_PATH = "${toolchain}/lib/rustlib/src/rust/library";
          };
        }
      );
    };
}
