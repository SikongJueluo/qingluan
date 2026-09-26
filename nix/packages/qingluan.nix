{
  lib,
  rustPlatform,
  frontend,
  root,
}:

# CLI (`qingluan`) + daemon (`qingluan-daemon`) from the cargo workspace.
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

  cargoBuildFlags = [
    "--package"
    "qingluan-cli"
    "--package"
    "qingluan-daemon"
  ];
  cargoTestFlags = finalAttrs.cargoBuildFlags;

  doCheck = false;

  meta = {
    description = "Qingluan CLI and daemon";
    mainProgram = "qingluan";
    platforms = lib.platforms.linux;
    license = lib.licenses.agpl3Plus;
  };
})
