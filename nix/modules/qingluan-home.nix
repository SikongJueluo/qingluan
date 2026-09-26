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
      default = pkgs.callPackage ../packages/qingluan.nix {
        root = ../..;
        frontend = pkgs.callPackage ../packages/frontend.nix { root = ../..; };
      };
      defaultText = lib.literalExpression "pkgs.callPackage ../packages/qingluan.nix { }";
      description = "The qingluan package providing the `qingluan` CLI (with the embedded daemon).";
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

    enableBashIntegration = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Whether to enable bash completions.";
    };

    enableFishIntegration = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Whether to enable fish completions.";
    };

    enableZshIntegration = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Whether to enable zsh completions.";
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages =
      [ cfg.package ]
      ++ lib.optionals cfg.desktop.enable [ cfg.desktop.package ];

    programs.bash.initExtra = lib.mkIf cfg.enableBashIntegration ''
      if [[ $- == *i* ]]; then
        source <(${lib.getExe' cfg.package "qingluan"} completions bash)
      fi
    '';

    programs.zsh.initExtra = lib.mkIf cfg.enableZshIntegration ''
      source <(${lib.getExe' cfg.package "qingluan"} completions zsh)
    '';

    programs.fish.interactiveShellInit = lib.mkIf cfg.enableFishIntegration ''
      ${lib.getExe' cfg.package "qingluan"} completions fish | source
    '';
  };
}
