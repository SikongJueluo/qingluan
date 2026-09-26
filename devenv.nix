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

  integrations.gitnr.".gitignore" = {
    templates = [
      "gh:Node"
      "gh:Rust"
      "gh:Nix"
    ];

    content = [
      # Devenv
      ".devenv*"
      "devenv.local.nix"
      "devenv.local.yaml"

      # direnv
      ".direnv"

      # pre-commit
      ".pre-commit-config.yaml"

      # others
      ".env"
      "**/dist-types/"
      "/target/"
      "**/target/"

      # protobuf IR cross-language roundtrip fixtures
      "packages/ir-proto/tests/fixtures/"

      # CLI test-generated entries that must resolve workspace packages
      "packages/cli/.test-fixtures/"
    ];
  };
}
