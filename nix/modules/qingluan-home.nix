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

    daemon.enable = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Whether to run qingluan-daemon as a delegated systemd user service.";
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

    systemd.user.services.qingluan-daemon = lib.mkIf cfg.daemon.enable {
      Unit = {
        Description = "Qingluan agent daemon";
        After = [ "graphical-session-pre.target" ];
      };
      Service = {
        Type = "simple";
        ExecStart = "${cfg.package}/bin/qingluan daemon start";
        Restart = "on-failure";
        RestartSec = 2;
        Delegate = true;
        KillMode = "mixed";
        TimeoutStopSec = 120;
        RuntimeDirectory = "qingluan";
        RuntimeDirectoryMode = "0700";
        UMask = "0077";
      };
      Install.WantedBy = [ "default.target" ];
    };
  };
}
