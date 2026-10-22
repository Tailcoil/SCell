{
  description = "More than fast: a clean, intuitive, all-in-one modern terminal";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
    }:
    let
      eachSystem =
        f:
        nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" ] (
          system:
          f (
            import nixpkgs {
              inherit system;
              overlays = [ rust-overlay.overlays.default ];
            }
          )
        );
      # 与 rustup 用同一个工具链版本；nixpkgs 自带的 rustc 可能低于 rust-version
      toolchain = pkgs: pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
    in
    {
      packages = eachSystem (pkgs: {
        default = pkgs.callPackage ./nix/package.nix {
          rustPlatform = pkgs.makeRustPlatform {
            cargo = toolchain pkgs;
            rustc = toolchain pkgs;
          };
        };
      });

      devShells = eachSystem (
        pkgs:
        let
          scell = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        in
        {
          default = pkgs.mkShell {
            inputsFrom = [ scell ];
            packages = [ (toolchain pkgs) ];
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath scell.runtimeLibs;
          };
        }
      );
    };
}
