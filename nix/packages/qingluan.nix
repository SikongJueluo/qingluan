{
  lib,
  protobuf,
  rustPlatform,
  frontend,
  root,
}:

# CLI (`qingluan`) — single entry point: client commands plus the embedded
# daemon (`qingluan daemon start`, served from the qingluan-daemon lib).
# The Tauri desktop app is packaged separately as qingluan-desktop to keep the
# GUI closure (webkitgtk etc.) out of headless installs.
rustPlatform.buildRustPackage (finalAttrs: {
  pname = "qingluan";
  version = "0.1.0";

  src = "${root}";

  # qingluan-daemon embeds apps/web/dist via include_dir!; the prebuilt
  # frontend derivation fills it in (dist is gitignored, hence absent from
  # the flake source).
  postPatch = ''
    mkdir -p apps/web/dist
    cp -r ${frontend}/. apps/web/dist/
  '';

  cargoLock.lockFile = ../../Cargo.lock;

  nativeBuildInputs = [ protobuf ];

  cargoBuildFlags = [
    "--package"
    "qingluan-cli"
  ];
  cargoTestFlags = finalAttrs.cargoBuildFlags;

  doCheck = false;

  # Standard completion dirs: NixOS/Home Manager users get these for free;
  # everyone else can `qingluan completions <shell>` (starship-style).
  postInstall = ''
    $out/bin/qingluan completions bash > qingluan.bash
    $out/bin/qingluan completions fish > qingluan.fish
    $out/bin/qingluan completions zsh > _qingluan
    install -Dm644 qingluan.bash $out/share/bash-completion/completions/qingluan
    install -Dm644 qingluan.fish $out/share/fish/vendor_completions.d/qingluan.fish
    install -Dm644 _qingluan $out/share/zsh/site-functions/_qingluan
  '';

  meta = {
    description = "Qingluan CLI with embedded daemon";
    mainProgram = "qingluan";
    platforms = lib.platforms.linux;
    license = lib.licenses.agpl3Plus;
  };
})
