{
  description = "dds (destroy_ds_stores) - A fast macOS .DS_Store cleaner with caching";

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
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };

        # Use the latest stable Rust with additional components
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" ];
        };

        # Native build inputs required for compilation
        nativeBuildInputs = with pkgs; [
          rustToolchain
          pkg-config
          cargo-edit
          cargo-watch
          cargo-outdated
          cargo-audit
          cargo-expand
          cargo-nextest
          sqlx-cli
        ];

        # Runtime dependencies
        buildInputs = with pkgs; [
          sqlite
          openssl
        ] ++ lib.optionals stdenv.isDarwin [
          darwin.apple_sdk.frameworks.Security
          darwin.apple_sdk.frameworks.SystemConfiguration
        ];

        # Development tools
        devTools = with pkgs; [
          # Task runner
          just

          # Benchmarking and testing
          hyperfine
          sqlite
          jq
          bc

          # Code quality
          tokei
          git
          pre-commit

          # Documentation
          mdbook
          graphviz
        ] ++ lib.optionals stdenv.isLinux [
          # Linux-only debugging tools
          gdb
          valgrind
        ] ++ lib.optionals stdenv.isDarwin [
          # macOS-only debugging tools
          lldb
        ];

      in
      {
        # Development shell
        devShells.default = pkgs.mkShell {
          inherit buildInputs nativeBuildInputs;

          packages = devTools;

          # Set up environment variables
          shellHook = ''
            echo "🦀 Welcome to dds development environment!"
            echo ""
            echo "Available commands:"
            echo "  just                 - Show available recipes"
            echo "  just i               - Install locally (cargo install --path=.)"
            echo "  just b               - Build release version"
            echo "  just d               - Build debug version"
            echo "  just t               - Run tests"
            echo "  just check           - Run all checks (fmt, clippy, test)"
            echo ""
            echo "Other tools:"
            echo "  hyperfine            - Performance benchmarking tool"
            echo "  sqlx                 - SQLx CLI for database management"
            echo ""
            echo "Test scripts:"
            echo "  ./tests/quick_test.sh       - Quick smoke test"
            echo "  ./tests/integration_test.sh - Full integration test"
            echo "  ./tests/benchmark.sh        - Performance benchmarks"
            echo ""

            # Set up database URL for development
            export DATABASE_URL="sqlite:dev.db"

            # Ensure rust-analyzer works properly
            export RUST_SRC_PATH="${rustToolchain}/lib/rustlib/src/rust/library"

            # Set up pre-commit hooks if not already done
            if [ ! -f .git/hooks/pre-commit ]; then
              echo "Setting up pre-commit hooks..."
              pre-commit install
            fi
          '';

          # Use the user's global cargo home for installations
          # CARGO_HOME = "./.cargo";  # Commented out to allow global installs

          # Enable backtrace for better error messages
          RUST_BACKTRACE = "1";
        };

        # Package definition
        packages = rec {
          dds = pkgs.rustPlatform.buildRustPackage {
            pname = "dds";
            version = "0.2.0";

            src = ./.;

            cargoLock = {
              lockFile = ./Cargo.lock;
            };

            inherit buildInputs nativeBuildInputs;

            # Skip tests during build (run them separately)
            doCheck = false;

            meta = with pkgs.lib; {
              description = "A fast macOS .DS_Store cleaner with SQLite caching";
              homepage = "https://github.com/yourusername/dds";
              license = licenses.mit;
              maintainers = [];
              platforms = platforms.darwin ++ platforms.linux;
            };
          };

          default = dds;
        };

        # Apps for nix run
        apps = rec {
          dds = flake-utils.lib.mkApp {
            drv = self.packages.${system}.dds;
          };
          default = dds;
        };

        # CI/CD checks
        checks = {
          # Format check
          fmt = pkgs.runCommand "cargo-fmt-check" {} ''
            cd ${./.}
            ${rustToolchain}/bin/cargo fmt --check
            touch $out
          '';

          # Clippy check
          clippy = pkgs.runCommand "cargo-clippy-check" {} ''
            cd ${./.}
            ${rustToolchain}/bin/cargo clippy -- -D warnings
            touch $out
          '';

          # Test check
          test = pkgs.runCommand "cargo-test-check" {} ''
            cd ${./.}
            ${rustToolchain}/bin/cargo test
            touch $out
          '';
        };
      }
    );
}
