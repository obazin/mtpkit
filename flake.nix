{
  description = "mtpkit — pure-Rust MTP library, CLI and (future) GUI for Android devices";

  # The toolchain pin lives in rust-projects, not here — nixpkgs' own rustc
  # moved with every input update before this. Do not add a rust-toolchain.toml:
  # the dev shell materializes the shared one at the project root so rustup and
  # Nix agree, and there is only ever one pin to bump.
  inputs.rust-projects.url = "github:obazin/rust-projects";

  outputs =
    { rust-projects, ... }:
    rust-projects.forEachSystem (
      lib:
      let
        # Virtual manifest — no [package] to read pname/version from, so the
        # version comes from the workspace table rather than a second copy.
        workspace = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace;
      in
      lib.mkRustProject {
        name = "mtpkit";
        src = ./.;

        pname = "mtpkit";
        version = workspace.package.version;

        meta = {
          description = "MTP client for Android devices: interactive shell and file transfer commands";
          license = lib.pkgs.lib.licenses.lgpl21Plus;
          # The binary the mtpkit-cli crate ships.
          mainProgram = "mtpkit";
        };
      }
    );
}
