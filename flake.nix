{
  description = "Booskiff — drive/storage service";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      in
      {
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            rustToolchain
            just
            jq
            sqlx-cli
            pkg-config
            openssl
            # aws-lc-rs compiles aws-lc from source and needs cmake + perl.
            cmake
            perl
          ];
          PKG_CONFIG_PATH = "${pkgs.openssl.dev}/lib/pkgconfig";
          LD_LIBRARY_PATH = "${pkgs.openssl.out}/lib";
        };

        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "booskiff";
          version = "0.1.0";
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;
          # aws-lc-rs builds the aws-lc C library via cmake.
          nativeBuildInputs = with pkgs; [ cmake perl ];
          doCheck = false;
        };
      });
}
