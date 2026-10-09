{
  description = "jihoo: an easy language with a GC'd VM and freestanding LLVM builds";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      lib = nixpkgs.lib;
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      version = "0.1.0";

      # Only what the compiler needs, so that editing the website (or docs)
      # does not rebuild it.
      src = lib.fileset.toSource {
        root = ./.;
        fileset = lib.fileset.unions [
          ./Cargo.toml
          ./Cargo.lock
          ./crates
          ./lib
          ./examples
          ./tests
        ];
      };

      meta = {
        homepage = "https://jihoo12.github.io/jihoo/";
        license = lib.licenses.asl20;
        platforms = lib.platforms.linux;
      };

      # Change the LLVM version here only.
      llvmFor = pkgs: pkgs.llvmPackages_21;

      mkPackages = pkgs:
        let llvm = llvmFor pkgs; in
        rec {
          # C++ LLVM backend: JIR text -> object file.
          jihoo-llc = llvm.stdenv.mkDerivation {
            pname = "jihoo-llc";
            inherit version;
            src = ./backend-llvm;
            nativeBuildInputs = [ pkgs.cmake pkgs.ninja ];
            buildInputs = [ llvm.llvm ];
            meta = meta // {
              description = "LLVM backend of the jihoo compiler";
              mainProgram = "jihoo-llc";
            };
          };

          # Rust frontend, VM and standard library. On its own it runs hosted
          # programs; `jihoo` below adds native builds.
          jihoo-frontend = pkgs.rustPlatform.buildRustPackage {
            pname = "jihoo-frontend";
            inherit version src;
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = [ pkgs.makeWrapper ];

            # Run the differential tests against the real backend too, so a build
            # only succeeds if the VM and native code agree.
            nativeCheckInputs = [ llvm.lld ];
            preCheck = ''
              export JIHOO_LLC=${jihoo-llc}/bin/jihoo-llc
            '';

            # Ship the standard library and point `import` at it.
            postInstall = ''
              mkdir -p $out/share/jihoo
              cp -r lib $out/share/jihoo/lib
              wrapProgram $out/bin/jihoo --suffix JIHOO_PATH : $out/share/jihoo/lib
            '';
            meta = meta // {
              description = "jihoo compiler frontend and VM";
              mainProgram = "jihoo";
            };
          };

          # The whole toolchain: `jihoo run` and `jihoo build` with no setup.
          jihoo = pkgs.runCommand "jihoo-${version}"
            {
              nativeBuildInputs = [ pkgs.makeWrapper ];
              meta = meta // {
                description = "jihoo toolchain: VM, LLVM backend, linker and standard library";
                mainProgram = "jihoo";
              };
            }
            ''
              mkdir -p $out/bin $out/share
              ln -s ${jihoo-frontend}/share/jihoo $out/share/jihoo
              ln -s ${jihoo-llc}/bin/jihoo-llc $out/bin/jihoo-llc
              makeWrapper ${jihoo-frontend}/bin/jihoo $out/bin/jihoo \
                --set-default JIHOO_LLC ${jihoo-llc}/bin/jihoo-llc \
                --set-default JIHOO_LD ${llvm.lld}/bin/ld.lld
            '';

          default = jihoo;
        };
    in
    {
      packages = forAllSystems mkPackages;

      overlays.default = final: prev: {
        inherit (mkPackages final) jihoo jihoo-frontend jihoo-llc;
      };

      apps = forAllSystems (pkgs: {
        default = {
          type = "app";
          program = "${self.packages.${pkgs.stdenv.hostPlatform.system}.jihoo}/bin/jihoo";
          meta.description = "Run the jihoo toolchain";
        };
      });

      # `nix flake check`: the installed toolchain, used from outside the source
      # tree, runs a hosted example and builds and runs native ones.
      checks = forAllSystems (pkgs:
        let jihoo = self.packages.${pkgs.stdenv.hostPlatform.system}.jihoo; in
        {
          toolchain = pkgs.runCommand "jihoo-toolchain-check" { nativeBuildInputs = [ jihoo ]; } (''
            # Outputs go to files: with pipefail, `| grep -q` would fail the
            # pipeline by closing it early.
            cp ${./examples}/*.jh .
            jihoo run hello.jh > hello.out
            grep -qx "Hello, jihoo!" hello.out
          '' + lib.optionalString pkgs.stdenv.hostPlatform.isx86_64 ''
            jihoo build freestanding.jh -o freestanding
            ./freestanding > /dev/null && status=0 || status=$?
            test "$status" = 55
            jihoo build arena.jh -o arena
            ./arena > arena.out || true
            grep -qx 332833500 arena.out
            jihoo build coroutines.jh -o coroutines
            ./coroutines > coroutines.out && status=0 || status=$?
            test "$status" = 4
            grep -qx 34 coroutines.out
          '' + ''
            touch $out
          '');
        });

      devShells = forAllSystems (pkgs:
        let llvm = llvmFor pkgs; in
        {
          default = (pkgs.mkShell.override { stdenv = llvm.stdenv; }) {
            nativeBuildInputs = [
              # Rust
              pkgs.cargo
              pkgs.rustc
              pkgs.rustfmt
              pkgs.clippy
              pkgs.rust-analyzer
              # C++ / LLVM
              pkgs.cmake
              pkgs.ninja
              llvm.clang-tools # clangd, clang-format
              llvm.lld # ld.lld for linking freestanding binaries
            ];
            buildInputs = [ llvm.llvm ];

            RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
          };

          # For working on the website in site/.
          site = pkgs.mkShell { packages = [ pkgs.nodejs ]; };
        });
    };
}
