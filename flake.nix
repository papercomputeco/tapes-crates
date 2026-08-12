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

        # A second nixpkgs, for the harness binaries the regression matrix
        # launches, and for nothing else.
        #
        # It exists only because some coding-agent CLIs ship under proprietary
        # licenses, and `allowUnfree` has to be set on the nixpkgs instance that
        # evaluates them. Setting it on the instance above would quietly relax
        # the licence policy for every other output in this flake, including the
        # dev shell contributors use; scoping it here keeps the exception to the
        # one place that needs it and makes it reviewable.
        #
        # Nothing in this repository builds against these — they are runtime
        # inputs to a test, and no crate here links a line of them.
        harnessPkgs = import nixpkgs {
          inherit system;
          config.allowUnfree = true;
        };
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

        # The harness regression matrix launches real harness binaries, so it
        # needs them on PATH. They live here rather than in the default shell
        # because they are large, they are irrelevant to every other target in
        # this repository, and paying for them on every `nix develop` would be a
        # poor trade for a shell whose usual job is `make check`.
        #
        # Whatever is missing is missing loudly: a harness that is not on PATH
        # produces a skip naming it, in the matrix's printed table and in the
        # version manifest the run emits. `pi` is not packaged in nixpkgs, so it
        # is the standing example — its cell skips here and runs on a developer
        # machine that has it installed.
        devShells.matrix = pkgs.mkShell {
          buildInputs = [
            rust
            pkgs.gnumake
            pkgs.git
            harnessPkgs.claude-code
            harnessPkgs.codex
            harnessPkgs.opencode
          ];

          shellHook = ''
            echo "tapes-harnesses matrix environment"
            echo ""
            echo "Harnesses on PATH:"
            for harness in claude codex opencode pi; do
              if command -v "$harness" >/dev/null 2>&1; then
                echo "  $harness: $("$harness" --version 2>&1 | head -1)"
              else
                echo "  $harness: absent (its cells will skip with a reason)"
              fi
            done
            echo ""
            echo "Run the matrix:"
            echo "  make harness-matrix"
          '';
        };
      }
    );
}
