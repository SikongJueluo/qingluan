{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.programs.qingluan;
in
{
  meta.maintainers = [ ];

  options.programs.qingluan = {
    enable = lib.mkEnableOption "Qingluan (CLI + daemon)";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ../packages/qingluan.nix { root = ../..; };
      defaultText = lib.literalExpression "pkgs.callPackage ../packages/qingluan.nix { }";
      description = "The qingluan package providing the `qingluan` CLI and `qingluan-daemon`.";
    };

    desktop = {
      enable = lib.mkEnableOption "the Qingluan Tauri desktop app";

      package = lib.mkOption {
        type = lib.types.package;
        default = pkgs.callPackage ../packages/qingluan-desktop.nix {
          root = ../..;
          frontend = pkgs.callPackage ../packages/frontend.nix { root = ../..; };
        };
        defaultText = lib.literalExpression "pkgs.callPackage ../packages/qingluan-desktop.nix { }";
        description = "The qingluan-desktop package.";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages =
      [ cfg.package ]
      ++ lib.optionals cfg.desktop.enable [ cfg.desktop.package ];
  };
}
