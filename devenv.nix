{
  pkgs,
  lib,
  config,
  ...
}: {
  # https://devenv.sh/packages/
  packages = [
    pkgs.cargo-tauri
    pkgs.cargo-watch
  ];

  # https://devenv.sh/languages/
  languages = {
    nix.enable = true;
    rust = {
      enable = true;
      lsp.enable = true;
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
