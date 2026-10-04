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

    nix-fast-build.url = "github:Mic92/nix-fast-build";
    nix-fast-build.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { self, nixpkgs, flake-utils, treefmt-nix, fenix, crane, advisory-db, nix-fast-build }:
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
            # Equivalence test cases are Nix source whose exact layout is part
            # of what they test.
            settings.global.excludes = [ "src/ogygia-nix-eval/tests/cases/**" ];
          };

          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              (craneLib.fileset.commonCargoSources ./.)
              ./src/ogygia-dashboard/src/web.css
              ./src/ogygia-clevis/tests/fixtures/sss.jwe
              ./src/ogygia-nix-eval/tests/cases
              ./src/ogygia-nix-eval-fuzz/README.md
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

          # ogygia's default `irisd` and `updated` features depend on the
          # ogygia-nixutils and ogygia-updated path crates, so their sources
          # must be present when building it.
          ogygiaSrc = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              (craneLib.fileset.commonCargoSources ./src/ogygia)
              (craneLib.fileset.commonCargoSources ./src/ogygia-nixutils)
              (craneLib.fileset.commonCargoSources ./src/ogygia-updated)
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
            src = ogygiaSrc;
            # Embed nebula-cert's store path in the binary so this derivation
            # carries a runtime dependency on nebula. Only set here, so plain
            # cargo builds (e.g. the dev shell) keep runtime PATH discovery.
            env.OGYGIA_NEBULA_CERT_BIN = "${pkgs.nebula}/bin/nebula-cert";
            nativeBuildInputs = individualCrateArgs.nativeBuildInputs ++ [ pkgs.installShellFiles ];
            # The completion shims are emitted by the binary itself, so this
            # runs after patchelf has made it loadable. Each shim embeds the
            # store path it was generated from, leaving no way for the
            # completions and the binary they call to drift apart.
            postInstall = ''
              ${pkgs.patchelf}/bin/patchelf --set-rpath "${lib.makeLibraryPath [ pkgs.openssl ]}" $out/bin/ogygia
              installShellCompletion --cmd ogygia \
                --bash <(COMPLETE=bash $out/bin/ogygia) \
                --zsh <(COMPLETE=zsh $out/bin/ogygia) \
                --fish <(COMPLETE=fish $out/bin/ogygia)
            '';
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

          ogygia-updated = craneLib.buildPackage (individualCrateArgs // {
            pname = "ogygia-updated";
            cargoExtraArgs = "-p ogygia-updated";
            src = fileSetForCrate ./src/ogygia-updated;
            postInstall = ''
              ${pkgs.patchelf}/bin/patchelf --set-rpath "${lib.makeLibraryPath [ pkgs.openssl ]}" $out/bin/ogygia-updated
            '';
          });

          ogygia-clevis = craneLib.buildPackage (individualCrateArgs // {
            pname = "ogygia-clevis";
            cargoExtraArgs = "-p ogygia-clevis";
            src = fileSetForCrate ./src/ogygia-clevis;
          });

          ogygia-dashboard = craneLib.buildPackage (individualCrateArgs // {
            pname = "ogygia-dashboard";
            cargoExtraArgs = "-p ogygia-dashboard";
            src = dashboardSrc;
            postInstall = ''
              ${pkgs.patchelf}/bin/patchelf --set-rpath "${lib.makeLibraryPath [ pkgs.openssl ]}" $out/bin/ogygia-dashboard
            '';
          });

          # cargo nextest archive builds with the test profile, so its
          # dependencies are cached separately from the release ones.
          testCargoArtifacts = craneLib.buildDepsOnly (commonArgs // {
            pname = "ogygia-test-deps";
            version = "git";
            CARGO_PROFILE = "test";
          });

          ogygia-nextest-archive = craneLib.buildPackage (commonArgs // {
            pname = "ogygia-nextest-archive";
            inherit version;
            cargoArtifacts = testCargoArtifacts;
            doCheck = false;
            doNotPostBuildInstallCargoBinaries = true;
            # Bake nebula-cert's and jj's store paths into the archived test
            # binaries the same way the ogygia package does, so the nebula
            # round-trip test and the ogygia-updated change-id tests find them
            # via the embedded constants when the archive runs — the test
            # runner has neither on PATH.
            env.OGYGIA_NEBULA_CERT_BIN = "${pkgs.nebula}/bin/nebula-cert";
            env.OGYGIA_JJ_BIN = "${pkgs.jujutsu}/bin/jj";
            env.OGYGIA_NIX_INSTANTIATE_BIN = "${pkgs.nix}/bin/nix-instantiate";
            env.OGYGIA_NIX_EVAL_DEFAULT_INCLUDE_PATH = "${nixIncludePath}";
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

          # The ogygia-nix-eval differential fuzzer, instrumented by cargo-fuzz.
          # Its use is described in src/ogygia-nix-eval-fuzz/README.md.
          ogygia-nix-eval-fuzz = craneLib.mkCargoDerivation (commonArgs // {
            pname = "ogygia-nix-eval-fuzz";
            inherit version;
            cargoArtifacts = null;
            doInstallCargoArtifacts = false;
            env = {
              OGYGIA_NIX_INSTANTIATE_BIN = "${pkgs.nix}/bin/nix-instantiate";
              OGYGIA_NIX_EVAL_DEFAULT_INCLUDE_PATH = "${nixIncludePath}";
              OGYGIA_NIX_EVAL_FUZZ_SEEDS = "${fuzzSeeds}";
              OGYGIA_NIX_EVAL_FUZZ_REV = self.rev or self.dirtyRev or "unknown";
            };
            nativeBuildInputs = commonArgs.nativeBuildInputs ++ [ pkgs.cargo-fuzz ];
            buildPhaseCargoCommand = ''
              cargo fuzz build --sanitizer none --release \
                --fuzz-dir src/ogygia-nix-eval-fuzz ogygia-nix-eval-fuzz
            '';
            installPhaseCommand = ''
              install -D -t $out/bin \
                target/${pkgs.stdenv.hostPlatform.rust.rustcTarget}/release/ogygia-nix-eval-fuzz
            '';
            meta.mainProgram = "ogygia-nix-eval-fuzz";
          });

          # The files Nix ships with itself, which `<nix/...>` paths find under
          # nix/. They are Nix's own (LGPL-2.1), so ogygia-nix-eval refers to
          # them by path rather than including them.
          nixIncludePath = pkgs.runCommand "nix-include-path" { meta.license = lib.licenses.lgpl21; } ''
            mkdir -p $out/nix
            # Nix embeds the file as a raw string literal opened on the line
            # before it (nix-meson-build-support/generate-header), so what it
            # serves starts with a newline.
            { echo; cat ${pkgs.nix.src}/src/libexpr/fetchurl.nix; } > $out/nix/fetchurl.nix
            cp ${pkgs.nix.src}/COPYING $out/
          '';

          # Real Nix code for the fuzzer to start from: our equivalence cases,
          # nixpkgs' lib, and rnix's parser tests. The dictionary adds the
          # name of every builtin of the pinned Nix.
          fuzzSeeds =
            let
              lock = builtins.fromTOML (builtins.readFile ./Cargo.lock);
              rnix = craneLib.downloadCargoPackage
                (lib.findFirst (p: p.name == "rnix") null lock.package);
            in
            pkgs.runCommand "ogygia-nix-eval-fuzz-seeds"
              { nativeBuildInputs = [ pkgs.nix pkgs.jq ]; }
              ''
                mkdir -p $out/seeds
                find ${./src/ogygia-nix-eval/tests/cases} ${nixpkgs}/lib \
                  ${rnix}/test_data/parser -name '*.nix' -print0 |
                  while IFS= read -r -d "" f; do
                    # Named by content, so duplicates are kept once.
                    seed=$out/seeds/$(sha1sum < "$f" | cut -c1-40)
                    [ -e "$seed" ] || cp "$f" "$seed"
                  done

                export HOME=$TMPDIR NIX_STATE_DIR=$TMPDIR/state
                cat ${./src/ogygia-nix-eval-fuzz/nix.dict} > $out/nix.dict
                nix-instantiate --eval --json --readonly-mode --store dummy:// \
                  --expr 'builtins.attrNames builtins' |
                  jq -r '.[] | "\"\(.)\""' >> $out/nix.dict
              '';

        in
        {
          packages = {
            inherit ogygia ogygia-irisd ogygia-hostinfod ogygia-dashboard ogygia-updated ogygia-clevis ogygia-nextest-archive ogygia-nix-eval-fuzz;
            default = ogygia;
          };

          devShells.default = craneLib.devShell {
            inputsFrom = [ cargoArtifacts ];
            OGYGIA_NIX_EVAL_DEFAULT_INCLUDE_PATH = "${nixIncludePath}";
            packages = with pkgs; [
              etcd # for etcdctl
              cargo-fuzz # for src/ogygia-nix-eval-fuzz
              jujutsu # jj, for the ogygia-updated change-id tests
              nebula # nebula-cert, for the nebula round-trip test
              rust-analyzer
              treefmtEval.config.build.wrapper
            ];
          };

          devShells.ci = pkgs.mkShell {
            packages = [
              toolchain
              pkgs.cargo-nextest
              nix-fast-build.packages.${system}.nix-fast-build
              pkgs.zstd
            ];
            # Doctests are not part of the nextest archive, so they are
            # compiled from source in this shell; give it the same build
            # inputs as the crate derivations so openssl-sys and prost
            # find their headers and tools.
            nativeBuildInputs = commonArgs.nativeBuildInputs;
            buildInputs = commonArgs.buildInputs;
            # Test binaries from the nextest archive link the nix openssl
            # dynamically but carry no usable runpath, and the nix dynamic
            # loader does not search the runner's system libraries.
            LD_LIBRARY_PATH = lib.makeLibraryPath [ pkgs.openssl ];
          };

          formatter = treefmtEval.config.build.wrapper;

          checks = self.packages.${system} // self.devShells.${system} // {

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

            # Compare ogygia-nix-eval against the pinned Nix on every case in
            # src/ogygia-nix-eval/tests/cases, including the nixpkgs lib suites,
            # and run the differential fuzzer's tests against it.
            ogygia-nix-eval-equiv = craneLib.cargoTest (commonArgs // {
              inherit cargoArtifacts;
              cargoTestExtraArgs = "-p ogygia-nix-eval -p ogygia-nix-eval-fuzz";
              env.OGYGIA_NIX_INSTANTIATE_BIN = "${pkgs.nix}/bin/nix-instantiate";
              env.OGYGIA_NIX_EVAL_DEFAULT_INCLUDE_PATH = "${nixIncludePath}";
              env.OGYGIA_NIX_EVAL_NIXPKGS = "${nixpkgs}";
            });

            ogygia-cli-config = import ./nixos/tests/cli-config.nix {
              inherit pkgs;
              inherit (nixpkgs) lib;
              ogygiaModule = self.nixosModules.default;
            };

            ogygia-updated-config = import ./nixos/tests/updated-config.nix {
              inherit pkgs;
              inherit (nixpkgs) lib;
              ogygiaModule = self.nixosModules.default;
            };

            ogygia-clevis-config = import ./nixos/tests/clevis-config.nix {
              inherit pkgs;
              inherit (nixpkgs) lib;
              ogygiaModule = self.nixosModules.default;
            };

            ogygia-nebula-module = import ./nixos/tests/nebula-module.nix {
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

            ogygia-clevis-sync = import ./nixos/tests/clevis-sync.nix {
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
        _module.args.ogygia-updated = self.packages.${pkgs.stdenv.hostPlatform.system}.ogygia-updated;
        _module.args.ogygia-clevis = self.packages.${pkgs.stdenv.hostPlatform.system}.ogygia-clevis;
      };
    };
}
