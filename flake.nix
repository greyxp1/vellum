{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  outputs = {
    self,
    nixpkgs,
  }: let
    systems = ["aarch64-linux" "x86_64-linux"];

    eachSystem = f:
      nixpkgs.lib.genAttrs systems
      (system: f system nixpkgs.legacyPackages.${system});
  in {
    packages = eachSystem (system: pkgs: {
      vellum = pkgs.callPackage ./nix/package.nix {};
      default = self.packages.${system}.vellum;
    });

    devShells = eachSystem (system: pkgs: {
      default = pkgs.mkShell {
        inputsFrom = [self.packages.${system}.vellum];
        packages = with pkgs; [
          cargo
          rustc
          rustfmt
          clippy
          rust-analyzer
        ];

        RUST_SRC_PATH = "${pkgs.rust.packages.stable.rustPlatform.rustLibSrc}";

        LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [
          pkgs.vulkan-loader
          pkgs.wayland
          pkgs.libxkbcommon
        ];
      };
    });

    homeModules.default = import ./nix/home-manager.nix {inherit self;};
  };
}
