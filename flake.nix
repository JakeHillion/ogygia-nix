{
  description = "Ogygia";

  nixConfig = {
    extra-substituters = [
      "https://nixcache.jakehillion.me"
    ];
    extra-trusted-public-keys = [
      "nixcache.jakehillion.me-1:HQsjYdrcs3ilS/ngtlbTQXU4Xfsm+va5NN7yoK0wKMg="
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
    flake-utils.lib.eachSystem [ "aarch64-linux" "x86_64-linux" ]
      (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          lib = pkgs.lib;
          toolchain = fenix.packages.${system}.combine [
            (fenix.packages.${system}.stable.withComponents [
              "cargo"
              "clippy"
              "rust-src"
              "rustc"
            ])
            (fenix.packages.${system}.complete.withComponents [
              "rustfmt"
            ])
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

          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              (craneLib.fileset.commonCargoSources ./.)
              ./src/ogygia-dashboard/src/web.css
            ];
          };
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

          dashboardSrc = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              (craneLib.fileset.commonCargoSources ./src/ogygia-dashboard)
              ./src/ogygia-dashboard/src/web.css
            ];
          };

          # ogygia-irisd depends on the ogygia-nixutils path crate, so both
          # crates' sources must be present when building it.
          irisdSrc = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              (craneLib.fileset.commonCargoSources ./src/ogygia-irisd)
              (craneLib.fileset.commonCargoSources ./src/ogygia-nixutils)
            ];
          };

          commonArgs = {
            inherit src;
            strictDeps = true;
            buildInputs = [ pkgs.openssl ];
            nativeBuildInputs = [ pkgs.protobuf pkgs.pkg-config ];
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

          # ogygia-nixutils is a library crate with no deployable artifact; it
          # is compiled (and linted/tested/documented) as part of ogygia-irisd
          # and the whole-workspace clippy/doc/nextest checks, so it gets no
          # standalone package of its own.

          ogygia-irisd = craneLib.buildPackage (individualCrateArgs // {
            pname = "ogygia-irisd";
            cargoExtraArgs = "-p ogygia-irisd";
            src = irisdSrc;
          });

          ogygia-hostinfod = craneLib.buildPackage (individualCrateArgs // {
            pname = "ogygia-hostinfod";
            cargoExtraArgs = "-p ogygia-hostinfod";
            src = fileSetForCrate ./src/ogygia-hostinfod;
          });

          ogygia-dashboard = craneLib.buildPackage (individualCrateArgs // {
            pname = "ogygia-dashboard";
            cargoExtraArgs = "-p ogygia-dashboard";
            src = dashboardSrc;
            postInstall = ''
              ${pkgs.patchelf}/bin/patchelf --set-rpath "${lib.makeLibraryPath [ pkgs.openssl ]}" $out/bin/ogygia-dashboard
            '';
          });

          ogygia-nextest-archive = craneLib.buildPackage (commonArgs // {
            pname = "ogygia-nextest-archive";
            inherit version cargoArtifacts;
            doCheck = false;
            doNotPostBuildInstallCargoBinaries = true;
            nativeBuildInputs = commonArgs.nativeBuildInputs ++ [
              pkgs.cargo-nextest
              pkgs.zstd
            ];
            buildPhase = ''
              runHook preBuild
              cargo nextest archive --workspace --archive-file archive.tar.zst
              # Decompress so Nix can scan the tar for store-path references and
              # retain the test binaries' runtime deps; compressed, they're hidden.
              unzstd archive.tar.zst
              runHook postBuild
            '';
            installPhase = ''
              runHook preInstall
              mkdir -p $out
              cp archive.tar $out/
              runHook postInstall
            '';
          });

        in
        {
          packages = {
            inherit ogygia ogygia-irisd ogygia-hostinfod ogygia-dashboard ogygia-nextest-archive;
            default = ogygia;
          };

          devShells.default = craneLib.devShell {
            checks = self.checks.${system};
            packages = with pkgs; [
              etcd # for etcdctl
              rust-analyzer
              treefmtEval.config.build.wrapper
            ];
          };

          devShells.ci = pkgs.mkShell {
            packages = [
              toolchain
              pkgs.cargo-nextest
              pkgs.zstd
            ];
          };

          formatter = treefmtEval.config.build.wrapper;

          checks = {
            inherit ogygia ogygia-irisd ogygia-hostinfod ogygia-dashboard;

            ogygia-clippy = craneLib.cargoClippy (commonArgs // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--all-targets -- --deny warnings";
            });

            ogygia-doc = craneLib.cargoDoc (commonArgs // {
              inherit cargoArtifacts;
              env.RUSTDOCFLAGS = "--deny warnings";
            });

            formatting = treefmtEval.config.build.check self;

            ogygia-audit = craneLib.cargoAudit {
              inherit src advisory-db;
            };

            ogygia-deny = craneLib.cargoDeny {
              inherit src;
            };

            ogygia-cli-config = import ./nixos/tests/cli-config.nix {
              inherit pkgs;
              inherit (nixpkgs) lib;
              ogygiaModule = self.nixosModules.default;
            };

          } // lib.optionalAttrs (system == "x86_64-linux") {
            ogygia-irisd-local = import ./nixos/tests/irisd-local.nix {
              inherit pkgs system;
              inherit (nixpkgs) lib;
              ogygiaModule = self.nixosModules.default;
            };

            ogygia-irisd-push = import ./nixos/tests/irisd-push.nix {
              inherit pkgs system;
              inherit (nixpkgs) lib;
              ogygiaModule = self.nixosModules.default;
              ogygia = self.packages.${system}.ogygia;
            };

            ogygia-hostinfod-inotify = import ./nixos/tests/hostinfod-inotify.nix {
              inherit pkgs system;
              inherit (nixpkgs) lib;
              ogygiaModule = self.nixosModules.default;
            };
          };
        }) // {
      nixosModules.default = { pkgs, ... }: {
        imports = [ ./nixos ];
        _module.args.ogygia-irisd = self.packages.${pkgs.stdenv.hostPlatform.system}.ogygia-irisd;
        _module.args.ogygia-hostinfod = self.packages.${pkgs.stdenv.hostPlatform.system}.ogygia-hostinfod;
        _module.args.ogygia-dashboard = self.packages.${pkgs.stdenv.hostPlatform.system}.ogygia-dashboard;
      };

      ci = nixpkgs.lib.genAttrs [ "aarch64-linux" "x86_64-linux" ] (system:
        (self.packages.${system} or { })
        // (self.checks.${system} or { })
        // (self.devShells.${system} or { })
      );
    };
}
