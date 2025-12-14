{
  description = "Ogygia";

  nixConfig = {
    extra-substituters = [
      "https://ogygia.cachix.org"
    ];
    extra-trusted-public-keys = [
      "ogygia.cachix.org-1:xb4bnMPeWgSP81Xs0Vl7ZU4Ez7Ul65qp/EoZ40pDaWo="
    ];
  };

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";

    treefmt-nix.url = "github:numtide/treefmt-nix";
    treefmt-nix.inputs.nixpkgs.follows = "nixpkgs";

    fenix.url = "github:nix-community/fenix";
    fenix.inputs.nixpkgs.follows = "nixpkgs";

    crane.url = "github:ipetkov/crane";

    advisory-db.url = "github:rustsec/advisory-db";
    advisory-db.flake = false;
  };

  outputs = { self, nixpkgs, flake-utils, treefmt-nix, fenix, crane, advisory-db }:
    let
      # Formatter and platform-agnostic outputs
      formatterOutputs = flake-utils.lib.eachDefaultSystem (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          toolchain = fenix.packages.${system}.stable.withComponents [
            "cargo"
            "clippy"
            "rust-src"
            "rustc"
            "rustfmt"
          ];

          treefmtEval = treefmt-nix.lib.evalModule pkgs {
            projectRootFile = "flake.nix";
            programs = {
              rustfmt = {
                enable = true;
                package = toolchain;
              };
              nixpkgs-fmt.enable = true;
            };
          };
        in
        {
          formatter = treefmtEval.config.build.wrapper;

          checks = {
            formatting = treefmtEval.config.build.check self;
          };
        });

      # Rust packages for Linux and Darwin
      rustOutputs = flake-utils.lib.eachSystem [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ]
        (system:
          let
            pkgs = nixpkgs.legacyPackages.${system};
            lib = pkgs.lib;
            toolchain = fenix.packages.${system}.stable.withComponents [
              "cargo"
              "clippy"
              "rust-src"
              "rustc"
              "rustfmt"
            ];
            craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;

            treefmtEval = treefmt-nix.lib.evalModule pkgs {
              projectRootFile = "flake.nix";
              programs = {
                rustfmt = {
                  enable = true;
                  package = toolchain;
                };
                nixpkgs-fmt.enable = true;
              };
            };

            src = craneLib.cleanCargoSource (craneLib.path ./.);
            inherit (craneLib.crateNameFromCargoToml { inherit src; }) version;

            fileSetForCrate = crate:
              lib.fileset.toSource {
                root = ./.;
                fileset = lib.fileset.unions [
                  ./Cargo.toml
                  ./Cargo.lock
                  (craneLib.fileset.commonCargoSources crate)
                ];
              };

            commonArgs = {
              inherit src;
              strictDeps = true;
              buildInputs = [ ];
              nativeBuildInputs = [ ];
            };

            individualCrateArgs = commonArgs // {
              inherit cargoArtifacts;
              inherit (craneLib.crateNameFromCargoToml { inherit src; }) version;
              doCheck = false;
            };

            cargoArtifacts = craneLib.buildDepsOnly (commonArgs // {
              pname = "ogygia-deps";
              version = "git";
            });

            ogygia = craneLib.buildPackage (individualCrateArgs // {
              pname = "ogygia";
              cargoExtraArgs = "-p ogygia";
              src = fileSetForCrate ./src/ogygia;
            });

          in
          {
            packages = {
              inherit ogygia;
              default = ogygia;
            };

            devShells.default = craneLib.devShell {
              checks = self.checks.${system} or { };
              packages = with pkgs; [
                rust-analyzer
                treefmtEval.config.build.wrapper
              ];
            };

            checks = {
              inherit ogygia;

              ogygia-clippy = craneLib.cargoClippy (commonArgs // {
                inherit cargoArtifacts;
                cargoClippyExtraArgs = "--all-targets -- --deny warnings";
              });

              ogygia-doc = craneLib.cargoDoc (commonArgs // {
                inherit cargoArtifacts;
                env.RUSTDOCFLAGS = "--deny warnings";
              });

              ogygia-audit = craneLib.cargoAudit {
                inherit src advisory-db;
              };

              ogygia-deny = craneLib.cargoDeny {
                inherit src;
              };

              ogygia-nextest = craneLib.cargoNextest (commonArgs // {
                inherit cargoArtifacts;
                partitions = 1;
                partitionType = "count";
                cargoNextestPartitionsExtraArgs = "--no-tests=pass";
              });

              ogygia-cli-config = import ./nixos/tests/cli-config.nix {
                inherit pkgs system;
                inherit (nixpkgs) lib;
                ogygiaModule = self.nixosModules.default;
              };
            };
          });

      # Darwin-only apps (macOS and iOS)
      darwinOutputs = flake-utils.lib.eachSystem [ "aarch64-darwin" ]
        (system:
          let
            pkgs = nixpkgs.legacyPackages.${system};
          in
          {
            packages = {
              macos-app = import ./apps/macos { inherit pkgs; };
              ios-app = import ./apps/ios { inherit pkgs; };
            };
          });

    in
    nixpkgs.lib.recursiveUpdate
      (nixpkgs.lib.recursiveUpdate formatterOutputs rustOutputs)
      (nixpkgs.lib.recursiveUpdate darwinOutputs {
        nixosModules.default = import ./nixos;
      });
}
