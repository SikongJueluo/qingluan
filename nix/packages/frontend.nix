{
  lib,
  stdenv,
  bun,
  fetchPnpmDeps,
  nodejs_22,
  pnpm_10,
  pnpmConfigHook,
  root,
}:

let
  pnpm = pnpm_10;
in
stdenv.mkDerivation (finalAttrs: {
  pname = "qingluan-frontend";
  version = "0.1.0";

  src = "${root}/apps/desktop/frontend";

  pnpmDeps = fetchPnpmDeps {
    inherit (finalAttrs) pname src;
    inherit pnpm;
    fetcherVersion = 3; # pnpm 10 store (v10); see nixpkgs manual #javascript-pnpm-fetcherVersion
    hash = "sha256-pkFY2E03j0YN/3KQELW4++GLKiznsd1Rv15OImWz58A=";
  };

  nativeBuildInputs = [
    bun
    nodejs_22
    pnpm
    pnpmConfigHook
  ];

  strictDeps = true;

  buildPhase = ''
    runHook preBuild

    pnpm install --offline --frozen-lockfile

    # runs `run-p type-check build-only` → vue-tsc + vite build
    bun run build

    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall

    mkdir -p $out
    cp -r dist/. $out/

    runHook postInstall
  '';

  meta = {
    description = "Qingluan desktop frontend (prebuilt static dist)";
    platforms = lib.platforms.linux;
    license = lib.licenses.agpl3Plus;
  };
})
