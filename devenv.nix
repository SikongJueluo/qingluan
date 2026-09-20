{
  pkgs,
  lib,
  config,
  ...
}: {
  # https://devenv.sh/packages/
  packages = with pkgs; [
    cargo-tauri
    cargo-watch
    pkg-config
    # Tauri 2 Linux system libraries (webkit2gtk-4.1 / javascriptcoregtk-4.1 /
    # libsoup-3.0 arrive via webkitgtk_4_1; devenv propagates their pkgconfig).
    glib
    gtk3
    webkitgtk_4_1
    librsvg
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
      bun.enable = true;
    };
  };

  # See full reference at https://devenv.sh/reference/options/
}
