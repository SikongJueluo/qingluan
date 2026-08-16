{
  pkgs,
  lib,
  config,
  ...
}:
{
  # https://devenv.sh/packages/
  packages = [
    pkgs.cargo-tauri
  ];

  # https://devenv.sh/languages/
  languages = {
    rust = {
      enable = true;
    };

    javascript = {
      enable = true;
      pnpm = {
        enable = true;
        install = {
          enable = true;
        };
      };
    };
  };

  # See full reference at https://devenv.sh/reference/options/
}
