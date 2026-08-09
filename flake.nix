{
  description = "mtpkit — pure-Rust MTP library, CLI and (future) GUI for Android devices";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs =
    { self, nixpkgs }:
    let
      lib = nixpkgs.lib;
      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: rec {
        default = mtpkit-cli;
        mtpkit-cli = pkgs.rustPlatform.buildRustPackage {
          pname = "mtpkit";
          version = "0.1.0";
          src = lib.cleanSource self;
          cargoLock.lockFile = ./Cargo.lock;
          meta = {
            description = "MTP client for Android devices: interactive shell and file transfer commands";
            license = lib.licenses.lgpl21Plus;
            mainProgram = "mtpkit";
          };
        };
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            rustfmt
            clippy
            rust-analyzer
          ];
          env.RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
        };
      });
    };
}
