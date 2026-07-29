{
  description = "tapes-harnesses — shared client-side harness knowledge for Tapes capture (Rust)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; overlays = [ (import rust-overlay) ]; };
        # Pin the toolchain to rust-toolchain.toml so `nix develop` and bare
        # `cargo` stay in lockstep.
        rust = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      in
      {
        # A library crate ships no binaries, so there is no package output —
        # consumers build it through cargo. The devShell is the whole story.
        devShells.default = pkgs.mkShell {
          buildInputs = [
            rust
            pkgs.gnumake
            pkgs.git
          ];

          shellHook = ''
            echo "tapes-harnesses development environment (Rust)"
            echo ""
            echo "Rust version: $(rustc --version)"
            echo ""
            echo "Available make targets:"
            make help
          '';
        };
      }
    );
}
