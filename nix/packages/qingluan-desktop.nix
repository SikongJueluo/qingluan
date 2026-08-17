{
  lib,
  rustPlatform,
  cargo-tauri,
  wrapGAppsHook3,
  pkg-config,
  dbus,
  glib-networking,
  gtk3,
  librsvg,
  openssl,
  webkitgtk_4_1,
  frontend,
  root,
}:

# Tauri 2 desktop app. The frontend is prebuilt by ./frontend.nix and wired in
# via tauri.conf.json substitution (pattern from nixpkgs `overlayed`), so the
# Rust build itself needs no node/pnpm/bun.
rustPlatform.buildRustPackage (finalAttrs: {
  pname = "qingluan-desktop";
  version = "0.1.0";

  src = "${root}";

  postPatch = ''
    # cargoSetupHook validates $cargoRoot/Cargo.lock; our lock lives at the
    # workspace root. An identical copy here is ignored by cargo itself.
    cp Cargo.lock apps/desktop/src-tauri/Cargo.lock

    substituteInPlace apps/desktop/src-tauri/tauri.conf.json \
      --replace-fail '"beforeBuildCommand": "bun run build"' '"beforeBuildCommand": ""' \
      --replace-fail '"frontendDist": "../frontend/dist"' '"frontendDist": "${frontend}"'
  '';

  cargoLock.lockFile = ../../Cargo.lock;

  nativeBuildInputs = [
    cargo-tauri.hook
    pkg-config
    wrapGAppsHook3
  ];

  buildInputs = [
    dbus
    glib-networking
    gtk3
    librsvg
    openssl
    webkitgtk_4_1
  ];

  cargoRoot = "apps/desktop/src-tauri";
  buildAndTestSubdir = finalAttrs.cargoRoot;

  doCheck = false;

  meta = {
    description = "Qingluan desktop app (Tauri)";
    homepage = "https://github.com/sikongjueluo/qingluan";
    mainProgram = "qingluan-desktop";
    platforms = lib.platforms.linux;
    license = lib.licenses.agpl3Plus;
  };
})
