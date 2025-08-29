{
  description = "Ogygia";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane = {
      url = "github:ipetkov/crane";
    };
    advisory-db = {
      url = "github:rustsec/advisory-db";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, flake-utils, fenix, crane, advisory-db }:
    flake-utils.lib.eachSystem [ "aarch64-linux" "x86_64-linux" ] (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        lib = pkgs.lib;
        toolchain = fenix.packages.${system}.stable.toolchain;
        craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;
        
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
          checks = self.checks.${system};
          packages = with pkgs; [
            rust-analyzer
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

          ogygia-fmt = craneLib.cargoFmt {
            inherit src;
          };

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
        };
      }) // {
    nixosModules.default = import ./module.nix;
  };
}
