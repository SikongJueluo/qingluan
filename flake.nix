{
  description = "Qingluan — agent task platform (CLI, daemon, Tauri desktop)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    devenv = {
      url = "github:cachix/devenv";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  # Binary caches (own cache to be added once created, see docs/research/cachix-distribution.md)
  nixConfig = {
    extra-substituters = [ "https://devenv.cachix.org" ];
    extra-trusted-public-keys = [
      "devenv.cachix.org-1:w1cLUi8dv3hnoSPGAuibQv+f9TZLr6cv/Hm9XgU50cw="
    ];
  };

  outputs =
    { self, nixpkgs, devenv, ... }@inputs:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      pkgsFor = system: nixpkgs.legacyPackages.${system};
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          frontend = pkgs.callPackage ./nix/packages/frontend.nix { root = self; };

          qingluan = pkgs.callPackage ./nix/packages/qingluan.nix { root = self; };

          qingluan-desktop = pkgs.callPackage ./nix/packages/qingluan-desktop.nix {
            root = self;
            frontend = self.packages.${system}.frontend;
          };

          default = self.packages.${system}.qingluan;
        }
      );

      apps = forAllSystems (system: {
        qingluan = {
          type = "app";
          program = nixpkgs.lib.getExe self.packages.${system}.qingluan;
        };
        qingluan-desktop = {
          type = "app";
          program = nixpkgs.lib.getExe self.packages.${system}.qingluan-desktop;
        };
        default = self.apps.${system}.qingluan;
      });

      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          default = devenv.lib.mkShell {
            inherit inputs pkgs;
            modules = [ (import ./devenv.nix) ];
          };
        }
      );

      nixosModules = {
        qingluan = import ./nix/modules/qingluan-system.nix;
        default = self.nixosModules.qingluan;
      };

      homeManagerModules = {
        qingluan = import ./nix/modules/qingluan-home.nix;
        default = self.homeManagerModules.qingluan;
      };
    };
}
