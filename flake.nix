{
  description = "jihoo programming language";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      # Change the LLVM version here only.
      llvmFor = pkgs: pkgs.llvmPackages_21;
    in
    {
      packages = forAllSystems (pkgs:
        let llvm = llvmFor pkgs; in
        rec {
          # Rust frontend + VM (`jihoo` binary)
          jihoo = pkgs.rustPlatform.buildRustPackage {
            pname = "jihoo";
            version = "0.1.0";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = [ pkgs.makeWrapper ];
            # Ship the standard library and point `import` at it.
            postInstall = ''
              mkdir -p $out/share/jihoo
              cp -r lib $out/share/jihoo/lib
              wrapProgram $out/bin/jihoo --suffix JIHOO_PATH : $out/share/jihoo/lib
            '';
          };

          # C++ LLVM backend (`jihoo-llc` binary)
          jihoo-llc = llvm.stdenv.mkDerivation {
            pname = "jihoo-llc";
            version = "0.1.0";
            src = ./backend-llvm;
            nativeBuildInputs = [ pkgs.cmake pkgs.ninja ];
            buildInputs = [ llvm.llvm ];
          };

          default = pkgs.symlinkJoin {
            name = "jihoo-toolchain";
            paths = [ jihoo jihoo-llc llvm.lld ];
          };
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
        });
    };
}
