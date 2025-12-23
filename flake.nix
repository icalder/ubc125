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
      forAllSys = nixpkgs.lib.genAttrs nixpkgs.lib.platforms.all;
    in
    {
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
              pkgs.socat
              pkgs.protobuf
              toolchain

              # We want the unwrapped version, "rust-analyzer" (wrapped) comes with nixpkgs' toolchain
              pkgs.rust-analyzer-unwrapped
            ];

            RUST_SRC_PATH = "${toolchain}/lib/rustlib/src/rust/library";
          };
        }
      );

      packages = forAllSys (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };
        in
        {
          ubc125 = pkgs.rustPlatform.buildRustPackage {
            pname = "ubc125";
            version = "0.1.0";

            src = pkgs.lib.cleanSource ./.;

            cargoLock.lockFile = ./Cargo.lock;

            nativeBuildInputs = [
              pkgs.pkg-config
              pkgs.protobuf
            ];
            # buildInputs = [ pkgs.openssl ]; # Example of adding a runtime dependency
          };
        }
      );
    };
}
