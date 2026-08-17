# Research: devenv 与 NixOS 打包/分发集成

> 调研日期：2026-02 · 目标：让 qingluan 成为「下游 flake 加一个 input，就能 `programs.qingluan.enable = true`」的可分发项目，且本地可测。
> 全部结论基于一手来源（devenv.sh 文档、cachix/devenv 源码、Home Manager 手册与源码、nixpkgs 手册与源码、Nix 手册）。

## TL;DR（推荐路线图）

1. **开发环境**：保留 devenv（`devenv.nix` + `devenv.yaml`）。给仓库补一个 `flake.nix`，用官方 `devenv.lib.mkShell` 把现有 devenv 配置挂成 `devShells.<system>.default`（也可以直接用 devenv CLI 生成的 `.devenv.flake.nix`，但那样无法暴露包和模块）。
2. **最终打包**：不用 devenv 打 Tauri 包。按 nixpkgs 官方 Tauri 文档，用 `rustPlatform.buildRustPackage` + **`cargo-tauri.hook`** + **`fetchPnpmDeps`/`pnpmConfigHook`**，产出 `packages.<system>.qingluan`。
3. **模块分发**：在 flake outputs 里暴露 `nixosModules.qingluan` 与 `homeManagerModules.qingluan`（同时可兼容暴露 `homeModules.qingluan`）。模块内按 Home Manager 官方惯例写 `programs.qingluan.enable`（`mkEnableOption`）+ `package`（`mkPackageOption`）。
4. **本地测试**：`nix build .#packages.x86_64-linux.qingluan` → `nix run` / `nix profile install` → 用下游测试 flake 里 `home-manager switch --flake .#...` 或 `nixos-rebuild switch --flake` 验证 `programs.qingluan.enable = true`。
5. **注意**：unfree（`devenv.yaml allow_unfree` / HM 私有 pkgs 的 `nixpkgs.config.allowUnfree`）、home-manager 以 NixOS module 方式引入（`useGlobalPkgs`/`useUserPackages`）、以及 **devenv-nixpkgs/rolling 与系统 nixpkgs 是两个不同的 nixpkgs**——打包和模块必须基于系统 nixpkgs 求值，devenv 的 pkgs 只用于开发 shell。

---

## 1. devenv 定位：开发 shell 工具，还是打包工具？

**结论**：devenv 的定位是**开发者环境（developer environments）**，不是通用包管理器；但它自 1.1 起提供了官方 **`outputs`** 机制，可以在 devenv 内用"每种语言最好的打包工具"产出 Nix 包。经典分界：**devShell（`devenv.lib.mkShell`）归 devenv，最终分发包用 nixpkgs 的 derivation 体系（`buildRustPackage` 等）**；devenv `outputs` 是两者之间的官方桥梁。

- 官方 README 自我定位："Fast, Declarative, Reproducible, and Composable **Developer Environments**"，功能列表把「Packaging and deployment」单列为 `OCI containers` 与 `Outputs`（"for packaging apps using each language's best tools (crate2nix, uv2nix, ...)"）。 [cachix/devenv README](https://github.com/cachix/devenv/blob/main/README.md)
- `outputs` 选项官方定义："Nix outputs for `devenv build` consumption."，类型为 `config.lib.types.outputOf lib.types.attrs`。 [src/modules/outputs.nix](https://github.com/cachix/devenv/blob/main/src/modules/outputs.nix)
- `devenv build` 官方命令："Build any attribute in devenv.nix."（README CLI 参考）。`devenv build outputs.rust-app` 只构建指定 output 并输出 store 路径。 [devenv README（Commands）](https://github.com/cachix/devenv/blob/main/README.md) · [Outputs - devenv](https://devenv.sh/outputs/)
- devenv 的 devShell 侧产物：`devenv.lib.mkShell` 返回 `config.shell // { ci = config.ciDerivation; inherit config; }`，即一个标准 devShell derivation + CI 派生。 [cachix/devenv flake.nix](https://github.com/cachix/devenv/blob/main/flake.nix)

## 2. 已用 devenv 的仓库如何获得 flake outputs

**结论**：两种官方方式——(a) 在仓库根加 `flake.nix`，用 `devenv.lib.mkShell`（可加任意 `packages`/`nixosModules`/`homeManagerModules` 等输出）；(b) flake-parts 方式，`imports = [ inputs.devenv.flakeModule ]`，用 `devenv.shells.default`。不写 flake.nix 时 devenv CLI 会自动生成 `.devenv.flake.nix`（仅含 devShell），无法分发包/模块。

官方最小 `flake.nix`（devenv.sh 原样）：

```nix
{
  inputs = {
    nixpkgs.url = "github:cachix/devenv-nixpkgs/rolling";
    devenv.url = "github:cachix/devenv";
  };

  nixConfig = {
    extra-trusted-public-keys = "devenv.cachix.org-1:w1cLUi8dv3hnoSPGAuibQv+f9TZLr6cv/Hm9XgU50cw=";
    extra-substituters = "https://devenv.cachix.org";
  };

  outputs = { self, nixpkgs, devenv, ... } @ inputs:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
    in
    {
      devShells.${system}.default = devenv.lib.mkShell {
        inherit inputs pkgs;
        modules = [
          ({ pkgs, config, ... }: {
            # This is your devenv configuration
            packages = [ pkgs.hello ];

            enterShell = ''
              hello
            '';

            processes.run.exec = "hello";
          })
        ];
      };
    };
}
```

[Using devenv with Nix Flakes](https://devenv.sh/guides/using-with-flakes/)

要点：

- **`devenv.lib` 的全部内容**（来自 flake.nix 源码）：`mkConfig`、`mkEval`、`mkShell`；另有 `modules = ./src/modules`（可直接 `import devenv.modules` 复用其模块系统）、`overlays.default`、`templates`、`flakeModule`（flake-parts 用）。 [cachix/devenv flake.nix](https://github.com/cachix/devenv/blob/main/flake.nix)
- `devenv.flakesIntegration` 选项："Tells if devenv is being imported by a flake.nix file"（默认按调用方式自动判定）。 [devenv.nix options 参考](https://devenv.sh/reference/options/)
- flake 集成下必须 `nix develop --no-pure-eval`（devenv 需要查工作目录等，纯求值会失败）；direnv 的 `.envrc` 也带 `--no-pure-eval`。 [Using devenv with Nix Flakes](https://devenv.sh/guides/using-with-flakes/)
- 多 shell / monorepo：`devShells.${system}.projectA = devenv.lib.mkShell {...}` 各配各的 modules。同上。
- flake-parts 方式最小示例（`devenv.shells.default` + `imports = [ inputs.devenv.flakeModule ]`，可 `imports = [ ./devenv.nix ]` 引入现有配置）： [Using with flake.parts](https://devenv.sh/guides/using-with-flake-parts/) · [devenv/flake-module.nix](https://github.com/cachix/devenv/blob/main/flake-module.nix)

## 3. Rust/Tauri 前后端项目如何打包（devenv 是否参与）

**结论**：推荐 nixpkgs 官方组合 `rustPlatform.buildRustPackage` + **`cargo-tauri.hook`** + `fetchPnpmDeps`/`pnpmConfigHook`。devenv **不参与**最终打包（除非走其 crate2nix 路线，Tauri 不推荐）。

### 3.1 Rust 侧：buildRustPackage + cargoLock

- 官方做法：`rustPlatform.buildRustPackage`，用 `cargoLock.lockFile = ./Cargo.lock` 免去每次改 lock 都更新 hash；git 依赖需 `outputHashes`；仓库缺 `Cargo.lock` 用 `cargoPatches` 补。 [nixpkgs manual: Rust](https://nixos.org/manual/nixpkgs/stable/#rust) · [rust.section.md](https://github.com/NixOS/nixpkgs/blob/master/doc/languages-frameworks/rust.section.md)
- Tauri 项目源码在 `src-tauri/` 子目录时：`cargoRoot = "src-tauri"; buildAndTestSubdir = finalAttrs.cargoRoot;`。

### 3.2 前端侧：pnpm

- 官方三件套：`fetchPnpmDeps`（FOD 拉 pnpm store）+ `pnpmConfigHook`（把 store 装进 `node_modules`，离线、无网络）+ `pnpmBuildHook`（跑 `pnpm build`）。**必须 pin pnpm 大版本**（`pnpm_11` 等，store 格式会变），并设置 `fetcherVersion`（3/4）。 [nixpkgs manual: JavaScript/pnpm](https://nixos.org/manual/nixpkgs/stable/#javascript-pnpm) · [pnpmBuildHook](https://nixos.org/manual/nixpkgs/stable/#hooks-pnpm) · [fetch-pnpm-deps 源码（fetcherVersion 校验）](https://github.com/NixOS/nixpkgs/blob/master/pkgs/build-support/node/fetch-pnpm-deps/default.nix)

### 3.3 Tauri 官方 hook（nixpkgs 文档原样示例）

> In Nixpkgs, `cargo-tauri.hook` overrides the default build and install phases.

```nix
rustPlatform.buildRustPackage (finalAttrs: {
  # ...

  cargoHash = "...";

  # Assuming our app's frontend uses `npm` as a package manager
  npmDeps = fetchNpmDeps {
    name = "${finalAttrs.pname}-${finalAttrs.version}-npm-deps";
    inherit (finalAttrs) src;
    hash = "...";
  };

  nativeBuildInputs = [
    cargo-tauri.hook          # 主 hook：编排 cargo tauri build
    nodejs
    npmHooks.npmConfigHook
    pkg-config
  ]
  ++ lib.optionals stdenv.hostPlatform.isLinux [ wrapGAppsHook4 ];

  buildInputs = lib.optionals stdenv.hostPlatform.isLinux [
    glib-networking           # Most Tauri apps need networking
    openssl
    webkitgtk_4_1
  ];

  cargoRoot = "src-tauri";
  buildAndTestSubdir = finalAttrs.cargoRoot;
  # ...
})
```

[doc/hooks/tauri.section.md（nixpkgs 官方文档）](https://github.com/NixOS/nixpkgs/blob/master/doc/hooks/tauri.section.md) · [NixOS Wiki: Tauri](https://wiki.nixos.org/wiki/Tauri)

### 3.4 成熟实例（可对照抄）

- **nixpkgs 正式包 `sone`**（Tauri 2 + pnpm）：`pnpm_11` + `fetchPnpmDeps { fetcherVersion = 4; }` + `cargo-tauri.hook` + `wrapGAppsHook3`；buildInputs 含 `webkitgtk_4_1, glib-networking, gtk3, librsvg, dbus, alsa-lib, gst_all_1.*, openssl`。 [nixpkgs: sone/package.nix](https://github.com/NixOS/nixpkgs/blob/nixpkgs-unstable/pkgs/by-name/so/sone/package.nix)
- **`overlayed`**（nixpkgs，前端独立 derivation）：`webui` 单独构建，`postPatch` 里 `substituteInPlace ./tauri.conf.json --replace-fail '../dist' '${webui}'`，并 patch `libappindicator-sys` 的 dlopen 路径为 store 绝对路径；`cargoLock.lockFile` + `outputHashes`。 [nixpkgs: overlayed/package.nix](https://github.com/NixOS/nixpkgs/blob/release-24.11/pkgs/by-name/ov/overlayed/package.nix)
- **NUR `futo-notes`**：`pnpm_10.fetchDeps` 写法 + `cargo-tauri.hook` + `buildAndTestSubdir = "apps/tauri/src-tauri"`，`preBuild` 手动 `pnpm install --offline --frozen-lockfile --ignore-scripts && pnpm run build`。 [nur-combined: futo-notes](https://github.com/nix-community/nur-combined/blob/main/repos/mio/by-name/fu/futo-notes/package.nix)
- **crane 路线**（tonisives/ovim flake）：pnpm deps FOD + 独立 frontend derivation + `craneLib.buildPackage`，`preConfigure` 把 `dist` 拷到 `src-tauri/../dist`；注释指出 Tauri 的 build.rs 会嵌入前端资源，故关闭 cargoArtifacts 缓存。 [ovim/flake.nix](https://github.com/tonisives/ovim/blob/main/flake.nix)

### 3.5 devenv 是否参与

- devenv `outputs` 的 Rust 走 **crate2nix**（`config.languages.rust.import ./rust-app {}`）。 [Outputs - devenv](https://devenv.sh/outputs/)
- Tauri 的特殊性（`tauri-build` 在编译期嵌入 `frontendDist`、webkitgtk 依赖、dlopen patch）与 crate2nix 兼容性差，官方 nixpkgs 已提供 `cargo-tauri.hook` 专门解决此问题——**最终包直接用 nixpkgs 体系，devenv 只负责 `devenv shell`（cargo-tauri、rust、node/pnpm 已在你的 devenv.nix 里）**。

## 4. 暴露 nixosModules / homeManagerModules，让下游 `programs.<name>.enable = true`

### 4.1 flake output 约定

- `nixosModules."<name>"` 与 `nixosModules.default` 是标准 flake output（Nix 2.8 起 `nixosModule` 更名为 `nixosModules.default`，旧名继续可用但有警告）。 [Nix 2.8 release notes](https://nix.dev/manual/nix/2.33/release-notes/rl-2.8) · [NixOS Wiki: Flakes（outputs schema）](https://wiki.nixos.org/wiki/Flakes)
- Home Manager 官方自身的 flake 就是范例：`nixosModules.home-manager` + `default`（同指 `./nixos`）、`darwinModules`、`flakeModules`。 [nix-community/home-manager flake.nix](https://github.com/nix-community/home-manager/blob/master/flake.nix)
- **`homeManagerModules.<name>`**：社区约定名（与 `nixosModules`/`darwinModules` 对齐，`class = "homeManager"`）。注意生态尚未完全统一：home-manager 的 flake-module 里正式选项名是 **`flake.homeModules`**（`types.lazyAttrsOf types.deferredModule`，apply 时打 `_class = "homeManager"` 标记，供 flake-parts 场景）；PR #6392 讨论过重命名 `homeModules` → `homeManagerModules`，社区两种都用。**稳妥做法：`homeManagerModules.qingluan` 为主，再顺带暴露 `homeModules.qingluan` 兼容 flake-parts/flake-schemas 用户。** [home-manager flake-module.nix](https://github.com/nix-community/home-manager/blob/master/flake-module.nix) · [PR #6392](https://github.com/nix-community/home-manager/pull/6392) · [Home Manager Manual（Nix Flakes 章）](https://nix-community.github.io/home-manager/)
- 下游引用方式（`inputs` + `imports`）：

```nix
{ inputs, ... }: {
  imports = [
    inputs.qingluan.nixosModules.qingluan        # 系统级
    inputs.qingluan.homeManagerModules.qingluan  # 用户级
  ];
  programs.qingluan.enable = true;               # ← 目标用法
}
```

### 4.2 module 内部典型结构（官方范例）

Home Manager 模块体系完全基于 NixOS 模块系统。 [Writing Home Manager Modules](https://nix-community.github.io/home-manager/index.xhtml#ch-writing-modules)（源码 [writing-modules.md](https://github.com/nix-community/home-manager/blob/master/docs/manual/writing-modules.md)）。HM 官方 `programs.*` 模块的 canonical 形状（`gallery-dl.nix` 全文）：

```nix
{ config, lib, pkgs, ... }:
let
  cfg = config.programs.gallery-dl;
  jsonFormat = pkgs.formats.json { };
in
{
  meta.maintainers = [ ];

  options.programs.gallery-dl = {
    enable = lib.mkEnableOption "gallery-dl";

    package = lib.mkPackageOption pkgs "gallery-dl" { nullable = true; };

    settings = lib.mkOption {
      inherit (jsonFormat) type;
      default = { };
      example = { extractor.base-directory = "~/Downloads"; };
      description = ''
        Configuration written to `$XDG_CONFIG_HOME/gallery-dl/config.json`.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = lib.mkIf (cfg.package != null) [ cfg.package ];

    xdg.configFile."gallery-dl/config.json" = lib.mkIf (cfg.settings != { }) {
      source = jsonFormat.generate "gallery-dl-settings" cfg.settings;
    };
  };
}
```

[home-manager: modules/programs/gallery-dl.nix](https://github.com/nix-community/home-manager/blob/master/modules/programs/gallery-dl.nix)

进阶参考：`programs.ghostty`（Tauri 类桌面 app 的 HM 模块好模板）演示了 **systemd user service**（`xdg.configFile."systemd/user/..."` + drop-in overrides）、**`dbus.packages`**（DBus 激活）、`assertions`（如 `cfg.systemd.enable -> cfg.package != null`）。 [home-manager: modules/programs/ghostty.nix](https://github.com/nix-community/home-manager/blob/master/modules/programs/ghostty.nix)

NixOS 侧选项惯例（`mkEnableOption` / `mkPackageOption`，NixOS 手册原样示例）：

```nix
options.services.myService = {
  enable = lib.mkEnableOption "my service";
  package = lib.mkPackageOption pkgs "my-service" {
    extraDescription = "Package used to run my service.";
  };
};
config = lib.mkIf cfg.enable {
  environment.systemPackages = [ cfg.package ];
};
```

[NixOS Manual: Writing NixOS Modules（Option Declarations）](https://nixos.org/manual/nixos/stable/#sec-writing-modules)

### 4.3 wrappers（分发时给可执行文件补运行环境）

- nixpkgs 手册的 wrapper 工具：`makeWrapper` / `makeBinaryWrapper`（`nativeBuildInputs = [ makeWrapper ]; postInstall = '' wrapProgram $out/bin/foo --prefix PATH : ${lib.makeBinPath [ hello git ]} ... ''`）。 [nixpkgs manual: Shell functions and utilities（makeWrapper）](https://github.com/NixOS/nixpkgs/blob/master/doc/stdenv/stdenv.chapter.md) · [Nixpkgs Reference Manual](https://nixos.org/manual/nixpkgs/stable/)
- GTK 应用必须用 `wrapGAppsHook3`/`wrapGAppsHook4`（Tauri 用 GTK3 → `wrapGAppsHook3`），否则运行时找不到 GSettings schemas / GIO 模块。同上 · [sone/package.nix 实例](https://github.com/NixOS/nixpkgs/blob/nixpkgs-unstable/pkgs/by-name/so/sone/package.nix)

## 5. 本地测试工作流

| 目的 | 命令 | 说明 |
| --- | --- | --- |
| 构建最终包 | `nix build .#packages.x86_64-linux.qingluan`（或 `nix build .#qingluan`） | 产出 `result/bin/qingluan` |
| 试运行 | `nix run .#qingluan` / `nix run .#qingluan -- --flag` | 对应 `apps.<system>.qingluan`（`type = "app"; program = ...`） |
| 装进用户 profile | `nix profile install .#qingluan` | 临时体验；正式分发应走 module |
| devenv 开发环境 | `devenv shell` 或 `nix develop --no-pure-eval .#default` | 开发用，与打包无关；`devenv build outputs.*` 只构建 devenv outputs |
| flake 完整性 | `nix flake check` | 校验 outputs schema（含 `nixosModules`/`homeManagerModules`） |
| HM 独立测试 | `home-manager build --flake .#sikongjueluo@host`（不激活，先看能否构建/激活脚本）→ `home-manager switch --flake .#sikongjueluo@host` | standalone 方式（`homeConfigurations` output） |
| 系统级测试 | `nixos-rebuild switch --flake .#hostname` | HM 以 NixOS module 方式随系统构建 |

[Home Manager Manual: Nix Flakes](https://nix-community.github.io/home-manager/) · [NixOS Wiki: Flakes](https://wiki.nixos.org/wiki/Flakes) · [devenv README（Commands: build）](https://github.com/cachix/devenv/blob/main/README.md)

要点：devenv shell 与 `nix profile install` 是**两条独立路径**——前者是开发时工具链，后者（以及 module 里的 `home.packages`）是分发产物；HM 有 generation/rollback（`home-manager switch --rollback`），系统级由 `nixos-rebuild` 管理回滚。

## 6. 常见坑

1. **unfree**：devenv 侧在 `devenv.yaml` 写 `allow_unfree: true`（生成 flake 时映射为 `allowUnfree`）。 [devenv.yaml reference](https://devenv.sh/reference/yaml-options/) · [.devenv.flake.nix 生成逻辑](https://github.com/riza-io/demos/blob/main/.devenv.flake.nix)；Home Manager 用**私有 pkgs 实例**，需在用户模块里 `home-manager.users.<name>.nixpkgs.config.allowUnfree = true`（HM 手册 NixOS module 章节注明其 pkgs 由 `home-manager.users.<name>.nixpkgs` 选项配置）。 [HM Manual: NixOS module](https://nix-community.github.io/home-manager/installation/nixos.html)
2. **home-manager standalone vs NixOS module**：非 NixOS 只能用 standalone；NixOS 上推荐 module 方式（随 `nixos-rebuild` 一起构建，用户 profile 与系统一致），用 `home-manager.useGlobalPkgs = true; home-manager.useUserPackages = true;` 让 HM 用系统 pkgs 且 profile 集成。 [HM Manual: Installation](https://nix-community.github.io/home-manager/) · [HM Manual: NixOS module（flake 最小示例）](https://nix-community.github.io/home-manager/installation/nixos.html)
3. **nixpkgs 版本对齐（devenv-nixpkgs rolling vs 系统 nixpkgs）**：
   - 新 devenv 项目默认用 **`github:cachix/devenv-nixpkgs/rolling`**——这是 nixpkgs-unstable 的 fork + devenv 集成补丁，"tested against devenv's test suite and receives monthly updates"；它与你系统 flake 的 nixpkgs **不是同一个**。 [devenv.sh/recipes/nix](https://devenv.sh/recipes/nix/) · [cachix/devenv-nixpkgs](https://github.com/cachix/devenv-nixpkgs)
   - `devenv.lock` 冻结 devenv 输入版本（同 `flake.lock` 角色），`devenv update` 才刷新；可用 `?ref=`/`?rev=` 精确 pin。 [Pinning - devenv](https://devenv.sh/pinning/)
   - **混用规则**：包/模块一律基于**系统 nixpkgs**（下游 flake 的 `inputs.nixpkgs`）求值；devenv 的 pkgs 只出现在 devShell。如果想让开发环境与系统对齐，在 flake 里 `devenv.inputs.nixpkgs.follows = "nixpkgs"`（devShell 用系统 nixpkgs），或反向 `--override-input nixpkgs github:NixOS/nixpkgs/nixos-unstable` 覆盖 devenv 输入（CLI 支持 `-o/--override-input`）。 [devenv README（Input overrides）](https://github.com/cachix/devenv/blob/main/README.md)
4. **Tauri 特有坑**：webkitgtk ABI（4.0/4.1/6.0 不能混，按 tauri.conf.json 匹配）；`libappindicator-sys` dlopen 硬编码 `.so.1` 需 `substituteInPlace` 到 store 路径（见 overlayed 实例）；`frontendDist` 要在 cargo 编译前存在（`cargo-tauri.hook` 或 preBuild 处理）。 [overlayed/package.nix](https://github.com/NixOS/nixpkgs/blob/release-24.11/pkgs/by-name/ov/overlayed/package.nix) · [Tauri Nix 打包注解](https://blog.a-stable.com/tauri2nix-notes)
5. **flake 纯求值**：devenv 集成下 `nix develop` 必须 `--no-pure-eval`；module 里不要依赖 devenv 的 `pkgs` 求值结果（版本与系统不一致会导致下游构建漂移）。

---

## 针对 qingluan 的落地建议

现状：`devenv.yaml`（input 仅 `nixpkgs: devenv-nixpkgs/rolling`）、`devenv.nix`（`cargo-tauri` + `languages.rust` + `languages.javascript.pnpm`）。适合做 Tauri 桌面应用的典型布局。

### 步骤 1：仓库根加 `flake.nix`（骨架）

```nix
{
  description = "qingluan";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    devenv.url = "github:cachix/devenv";
    # 可选：让 devShell 与系统 nixpkgs 对齐（dev 环境用系统包集合）
    devenv.inputs.nixpkgs.follows = "nixpkgs";
  };

  nixConfig = {
    extra-trusted-public-keys = "devenv.cachix.org-1:w1cLUi8dv3hnoSPGAuibQv+f9TZLr6cv/Hm9XgU50cw=";
    extra-substituters = "https://devenv.cachix.org";
  };

  outputs = { self, nixpkgs, devenv, ... }@inputs:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
    in
    {
      # 开发环境：复用现有 devenv.nix
      devShells.${system}.default = devenv.lib.mkShell {
        inherit inputs pkgs;
        modules = [ (import ./devenv.nix) ];
      };

      # 最终包
      packages.${system} = {
        qingluan = pkgs.callPackage ./nix/packages/qingluan.nix { };
        default = self.packages.${system}.qingluan;
      };

      apps.${system}.qingluan = {
        type = "app";
        program = "${self.packages.${system}.qingluan}/bin/qingluan";
      };

      # 模块分发
      nixosModules.qingluan = import ./nix/modules/qingluan-system.nix;
      homeManagerModules.qingluan = import ./nix/modules/qingluan-home.nix;
      homeModules.qingluan = self.homeManagerModules.qingluan; # 兼容 flake-parts/flake-schemas
    };
}
```

### 步骤 2：`nix/packages/qingluan.nix`（nixpkgs 官方 Tauri 组合）

```nix
{ lib, rustPlatform, fetchPnpmDeps, pnpmConfigHook, cargo-tauri, nodejs
, pnpm_11, pkg-config, wrapGAppsHook3, gtk3, webkitgtk_4_1, glib-networking
, openssl, dbus, librsvg }:

rustPlatform.buildRustPackage (finalAttrs: {
  pname = "qingluan";
  version = "0.1.0";

  src = lib.cleanSource ./.;

  cargoLock = {
    lockFile = ./src-tauri/Cargo.lock;
    # git 依赖必须给 outputHashes，构建报错会提示缺哪个
    # outputHashes = { "some-git-crate-0.1.0" = "sha256-..."; };
  };

  pnpmDeps = fetchPnpmDeps {
    inherit (finalAttrs) pname version src;
    pnpm = pnpm_11;
    fetcherVersion = 4;          # 与 nixpkgs 版本匹配，见 manual #javascript-pnpm-fetcherVersion
    hash = "sha256-...";         # 先填 "" 跑 nix build 拿真实 hash
  };

  nativeBuildInputs = [
    cargo-tauri.hook             # 编排前端构建 + cargo build + 安装
    nodejs
    pnpm_11
    pnpmConfigHook
    pkg-config
    wrapGAppsHook3               # Tauri 是 GTK3 应用
  ];

  buildInputs = [
    dbus gtk3 webkitgtk_4_1 glib-networking librsvg openssl
  ];

  cargoRoot = "src-tauri";
  buildAndTestSubdir = finalAttrs.cargoRoot;

  meta = {
    description = "qingluan";
    mainProgram = "qingluan";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
  };
})
```

> 若 `cargo-tauri.hook` 不自动跑前端构建（取决于 tauri.conf.json 配置），在 `preBuild` 里补 `pnpm run build`（NUR futo-notes 的做法）。

### 步骤 3：`nix/modules/qingluan-home.nix`（HM 模块）

```nix
{ config, lib, pkgs, ... }:
let
  cfg = config.programs.qingluan;
in
{
  meta.maintainers = [ ];

  options.programs.qingluan = {
    enable = lib.mkEnableOption "qingluan";

    package = lib.mkPackageOption pkgs "qingluan" { nullable = true; };

    autostart = lib.mkEnableOption "the qingluan systemd user service";
  };

  config = lib.mkIf cfg.enable {
    home.packages = lib.mkIf (cfg.package != null) [ cfg.package ];

    # 按需：配置文件、桌面集成
    xdg.configFile."qingluan/config.json" = {
      text = builtins.toJSON { };
    };

    systemd.user.services.qingluan = lib.mkIf cfg.autostart {
      Unit = {
        Description = "qingluan daemon";
        After = [ "graphical-session.target" ];
      };
      Service = {
        ExecStart = "${lib.getExe cfg.package} --daemon";
        Restart = "on-failure";
      };
      Install = { WantedBy = [ "graphical-session.target" ]; };
    };

    dbus.packages = lib.mkIf (cfg.package != null) [ cfg.package ]; # DBus 激活（ghostty 模式）
  };
}
```

`nix/modules/qingluan-system.nix` 同理（NixOS 侧）：`enable` + `package`，`config = lib.mkIf cfg.enable { environment.systemPackages = [ cfg.package ]; }`（Tauri 的 `.desktop` 与图标由 `cargo-tauri.hook` 装进 `$out/share/applications`，无需手写）。

### 步骤 4：本地验证序列

```bash
nix flake check
nix build .#qingluan && ./result/bin/qingluan --help
nix run .#qingluan
# 下游测试：另建 ~/tmp/qingluan-consumer 的 flake，inputs 加 qingluan（path: 或 github:），
# 在 home-manager.users.<name> 里 imports = [ inputs.qingluan.homeManagerModules.qingluan ]
# 然后 programs.qingluan.enable = true; 再：
home-manager switch --flake .#sikongjueluo@host   # standalone 快速验证
nixos-rebuild switch --flake .#hostname           # 系统级验证
```

### 建议的仓库结构

```
flake.nix                     # devShell + packages + apps + 两个模块输出
devenv.nix / devenv.yaml      # 保持现状（开发环境）
nix/
  packages/qingluan.nix       # buildRustPackage + cargo-tauri.hook + pnpm
  modules/qingluan-home.nix   # programs.qingluan (HM)
  modules/qingluan-system.nix # programs.qingluan (NixOS)
```

---

## Sources

- Kept:
  - devenv.sh: Using devenv with Nix Flakes (https://devenv.sh/guides/using-with-flakes/) — Q2 官方示例与 `--no-pure-eval` 说明
  - devenv.sh: Using with flake.parts (https://devenv.sh/guides/using-with-flake-parts/) — flakeModule 集成
  - devenv.sh: Outputs (https://devenv.sh/outputs/) — devenv 打包定位、`devenv build`、语言 import
  - devenv.sh: reference/options (https://devenv.sh/reference/options/) — `outputs`/`devenv.flakesIntegration` 选项
  - devenv.sh: Pinning / Inputs / recipes/nix (https://devenv.sh/pinning/) — devenv-nixpkgs rolling 与锁文件
  - cachix/devenv: flake.nix、src/modules/outputs.nix、flake-module.nix、README (https://github.com/cachix/devenv) — lib/modules 源码
  - cachix/devenv-nixpkgs (https://github.com/cachix/devenv-nixpkgs) — rolling 分支定位
  - Home Manager Manual（Nix Flakes / NixOS module / Writing modules）(https://nix-community.github.io/home-manager/) — homeConfigurations、useGlobalPkgs、模块体系
  - home-manager: flake.nix、flake-module.nix、writing-modules.md、programs/gallery-dl.nix、programs/ghostty.nix (https://github.com/nix-community/home-manager) — homeManagerModules/homeModules 约定与 canonical 模块
  - home-manager PR #6392 (https://github.com/nix-community/home-manager/pull/6392) — homeModules vs homeManagerModules 命名分歧
  - nixpkgs manual: Rust、JavaScript/pnpm、tauri.section.md、stdenv(makeWrapper) (https://nixos.org/manual/nixpkgs/stable/) — 打包三件套官方文档
  - nixpkgs: sone/package.nix、overlayed/package.nix、fetch-pnpm-deps/default.nix (https://github.com/NixOS/nixpkgs) — Tauri 成熟实例
  - NixOS Manual: Writing NixOS Modules (https://nixos.org/manual/nixos/stable/) — mkEnableOption/mkPackageOption
  - Nix 2.8 release notes (https://nix.dev/manual/nix/2.33/release-notes/rl-2.8) — nixosModule → nixosModules.default 更名
  - NixOS Wiki: Flakes / Tauri (https://wiki.nixos.org/wiki/Flakes) — outputs schema、Tauri 依赖
- Dropped:
  - AppSignal / pkgx / asdf / Erlang.mk 等泛泛文章 — 与主题无关
  - DeepWiki/Grokipedia 二手综述 — 仅作交叉印证，不作为结论来源
  - Determinate Systems blog — 与官方文档重复，无增量信息

## Gaps

- devenv `outputs` 的 Rust 路线（crate2nix）与 Tauri（build.rs 嵌入前端、webkitgtk）的实际兼容性未实测；文档标注 "Added in 1.1"，与 README 2.0 的版本关系未深究。建议后续在 qingluan 上做一次 `devenv build outputs.*` 小实验。
- `homeManagerModules` vs `homeModules` 尚无统一标准（社区 split，PR #6392 未合并），本文采用「主暴露 homeManagerModules + 别名 homeModules」的兼容方案，需在下游实测 `nix flake check` 无 unknown output 警告。
- 本文为资料调研，未在本机实际执行构建/switch；`fetchPnpmDeps` hash、`outputHashes`、webkitgtk 版本需按 qingluan 实际 lockfile 填充。
