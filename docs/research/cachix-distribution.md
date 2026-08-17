# Research: Cachix 接入（构建缓存分发）

> 调研日期：2026-02 · 目标：为 qingluan（Rust + Tauri + devenv + bun/pnpm 前端）接入 Cachix，让下游用户（flake inputs 引用者、CI、个人多台机器）直接替换二进制产物、免本地编译。
> 承接 `docs/research/devenv-nixos-packaging.md`（flake/打包/模块分发），本报告只聚焦 Cachix。
> 全部结论基于一手来源：docs.cachix.org 官方文档、cachix/cachix 与 cachix/cachix-action 仓库 README/源码、cachix/devenv 源码与文档、Nix 官方手册（nix.conf / flake format）。

## TL;DR（针对 qingluan 的接入路线图）

1. **建 cache**：在 [app.cachix.org](https://app.cachix.org) 注册 → 创建公共 cache（建议名 `qingluan`，与账号同名，名字会出现在下游所有用户的 Nix 配置里）。**默认托管签名（managed cache）**：public key 与 push 方式在 cache 页面直接给出，私钥由 Cachix 保管，个人无需 `generate-keypair`。在 cache Settings 里生成**一个 per-cache auth token**（只对该 cache 有写权限，比 personal token 安全），存进 GitHub Actions secrets（`CACHIX_AUTH_TOKEN`）。
2. **推送**：本地 `nix build --no-link --print-out-paths | cachix push qingluan`；CI 用 `cachix/cachix-action@v17`（push + pull，PR 上 `skipPush: true`）；devenv 侧在 CI 里写 `devenv.local.nix` 启用 `cachix.push = "qingluan"`，`devenv test` 构建 shell 时自动推送。devenv 默认已从 `devenv.cachix.org` 拉取（镜像 devenv-nixpkgs/rolling，我们的 `devenv.yaml` 正是用这个输入，**CI 不配这个 cache 会从源码构建 stdenv**）。
3. **下游消费**：仓库根 `flake.nix` 加 `nixConfig.extra-substituters` + `extra-trusted-public-keys`（用户首次构建会被交互询问是否信任，或全局 `accept-flake-config = true`）；NixOS 用户则在 `nix.settings` 里配。devenv 用户（开发者）在 `devenv.nix` 里 `cachix.pull = [ "qingluan" ]`。
4. **成本**：开源项目免费 5 GB 压缩存储、带宽不限。qingluan 的 devShell + Tauri 包闭包（排除 cache.nixos.org 已有条目）远小于此；超限后 Cachix 按 LRU 自动清最旧条目。
5. **注意**：cache 是按 store path 哈希寻址的全局扁平命名空间（不是按 commit/分支）；`devenv-nixpkgs/rolling` 每日滚动更新会让闭包哈希变化、触发重新构建，这是正常现象；push 不会覆盖已有条目；token/signing key 拥有 cache 完全读写权，只给 CI、不用 personal token、别在 fork PR 上启用 push。

---

## 1. Cachix 工作原理：binary cache、substituters、trusted public keys

**结论**：Cachix 是托管的 Nix binary cache。Nix 每个 store path 由内容哈希唯一标识；推送方把构建产物的 NAR 压缩上传到 cache 并签名；拉取方把 cache URL 配成 `substituters`、把签名公钥配进 `trusted-public-keys`，Nix 在构建前先尝试从 substituters 下载，下载的产物必须被某个受信公钥签名才被接受。

- "Cachix provides hosted binary caches so you can store and share your own build results… Push build outputs from CI so your team never rebuilds the same thing twice… Developers run `cachix use mycache` to configure Nix to pull from your cache." [What is a Binary Cache? - Cachix docs](https://docs.cachix.org/what-is-a-binary-cache)
- Nix 手册（nix.conf）：`substituters` 是"要使用的 store 地址列表"；`trusted-public-keys` 是"受信公钥列表"。接受一个 store 对象必须满足**至少一条**：它被某个 `trusted-public-keys` 里的公钥签名、或该 substituter 在 `trusted-substituters` 列表、或调用者是 `trusted-users`。 [Nix manual: nix.conf](https://nix.dev/manual/nix/stable/command-ref/conf-file#conf-substituters)
- `trusted-users`："These users will have additional rights when connecting to the Nix daemon, such as the ability to specify additional substituters, or to import unsigned realisations…" —— 非 trusted user 想临时指定 substituter，只能走 `trusted-substituters` 白名单，否则被忽略。 [Nix manual: nix.conf (trusted-users)](https://nix.dev/manual/nix/stable/command-ref/conf-file#conf-trusted-users)
- 安全模型（必须写进文档的警告）："Nix will accept any requested store object signed with private keys corresponding to the configured public keys. Access to those private keys thus allows substituting arbitrary files into your Nix store… Only add public keys you trust unconditionally." [nix.dev: Configure Nix to use a custom binary cache](https://nix.dev/guides/recipes/add-binary-cache)
- `cachix use <cache>` 的作用：把 substituters 与 trusted-public-keys 写进配置。官方 FAQ："If you're using NixOS, it will write NixOS configuration. If you're a trusted-user it will append to `~/.config/nix/nix.conf`. Otherwise it will either fail… or it will append to `/etc/nix/nix.conf`." [Cachix FAQ](https://docs.cachix.org/faq)

---

## 2. 创建与认证：cache 命名、auth token、public key、signing key

**结论**：登录 [app.cachix.org](https://app.cachix.org) 后即可创建 cache；名字选描述性的（`qingluan`），因为会出现在下游所有使用者的 Nix 配置中。认证用两类 token：personal（全账户权限）与 per-cache（只读写指定 cache，官方推荐隔离）。public key 由创建 cache 时生成的签名密钥对给出——默认模式（managed）下私钥由 Cachix 托管，页面直接显示公钥与 push 命令；高级模式（self-signed）才需要本地 `generate-keypair` 并自行保管私钥。

- "After logging into Cachix you'll be able to create a new binary cache."；"Choose a descriptive name for your cache (e.g. `myorg`, `myorg-private`, `myproject`) since the name appears in your Nix configuration and is visible to anyone using it." [Getting Started - Cachix docs](https://docs.cachix.org/getting-started)
- 两类 token：Personal——"full access to your account"；Per-cache——"allow write and/or read access to only a specific binary cache. On dashboard you can click on your newly generated binary cache Settings and generate a new access token." 设置方式：`cachix authtoken XXX` 或 `export CACHIX_AUTH_TOKEN=XXX`。 [Getting Started - Cachix docs](https://docs.cachix.org/getting-started)
- 签名模式：managed（默认，推荐）——"Cachix will manage the entire signing process for you… Cachix will sign the store paths with this key once they're pushed"；self-signed——"you create and manage your own signing key locally. Signing happens on the machine pushing the store paths." [Security - Cachix docs](https://docs.cachix.org/security)
- 自签名密钥生成（只有选了 self-signed 才需要）：`cachix authtoken <token>` → `cachix generate-keypair <cache>`，"The signing key is generated locally on your computer and is printed out to stdout. **This is the only copy, so make sure to create a backup.**"；Cachix 自动读取刚写入的本地 key，或通过环境变量 `$CACHIX_SIGNING_KEY` 传入（CI 用这个）。 [Getting Started - Cachix docs](https://docs.cachix.org/getting-started)
- 验证环境：`cachix doctor`（自 1.10 起）检查安装、配置、认证。 [Cachix FAQ](https://docs.cachix.org/faq)

---

## 3. 推送路径（官方推荐写法）

### 3a. 本地手动

**结论**：本地推送三件套——`nix build` 产出的 store path 通过管道喂给 `cachix push`；构建过程中边构建边推用 `cachix watch-exec`；长期驻留推整个 store 的新增路径用 `cachix watch-store`。官方文档原样：

```bash
# 经典 nix-build
$ nix-build | cachix push mycache

# Flake：推默认包的运行时闭包
$ nix build --no-link --print-out-paths \
  | cachix push mycache

# Flake：推任意一组包
$ nix build --no-link --print-out-paths .#package-a .#package-b \
  | cachix push mycache

# 构建过程中实时推（watch-exec 推该命令产生的所有新路径）
$ cachix watch-exec mycache -- nix-build --max-jobs 4

# 驻留进程，推之后所有新构建路径
$ cachix watch-store mycache

# 推 devShell 闭包（devenv 场景）
$ nix develop --profile dev-profile -c true
$ cachix push mycache dev-profile

# 把 flake inputs 也存进 cache（防止上游删除）
$ nix flake archive --json \
  | jq -r '.path,(.inputs|to_entries[].value.path)' \
  | cachix push mycache
```

[Pushing to Cachix - Cachix docs](https://docs.cachix.org/pushing) · [Getting Started - Cachix docs](https://docs.cachix.org/getting-started)

### 3b. GitHub Actions：cachix-action（+ DeterminateSystems/nix-installer-action）

**结论**：`cachix/cachix-action@v17` 一个 step 完成「配置 substituter + 装 cachix + 按需 push」。写权限两种：`authToken`（推任意 cache / 私库访问都要）和 `signingKey`（self-signed cache 才需要，与 authToken 叠加）。关键输入：`name`（必填）、`extraPullNames`（逗号分隔的额外 pull cache）、`skipPush`（只拉不推，默认 `false`）、`useDaemon`（默认 `true`，用 post-build hooks 边构建边推）、`pathsToPush`、`pushFilter`。 [cachix/cachix-action README](https://github.com/cachix/cachix-action) · [action.yml](https://github.com/cachix/cachix-action/blob/b4341580790b4a5d440dbca1d34a1d35a1261d37/action.yml)

```yaml
# 只读（公共 cache，无 secrets）
- uses: cachix/cachix-action@v17
  with:
    name: mycache

# 写：auth token（managed cache 官方推荐）
- uses: cachix/cachix-action@v17
  with:
    name: mycache
    authToken: "${{ secrets.CACHIX_AUTH_TOKEN }}"

# 写：auth token + 自签名密钥（self-signed cache）
- uses: cachix/cachix-action@v17
  with:
    name: mycache
    authToken: "${{ secrets.CACHIX_AUTH_TOKEN }}"
    signingKey: "${{ secrets.CACHIX_SIGNING_KEY }}"

# 拉多个 cache（例如同时用 devenv 与自己的）
- uses: cachix/cachix-action@v17
  with:
    name: qingluan
    authToken: "${{ secrets.CACHIX_AUTH_TOKEN }}"
    extraPullNames: devenv
```

- 与 DeterminateSystems/nix-installer-action 组合：`nix-installer-action` 负责装 Nix（自动启用 `nix-command`/`flakes`、`auto-optimise-store`、KVM 等），`cachix-action` 必须放在它**之后**；也可以不加 `authToken` 只用来 pull。 [DeterminateSystems/nix-installer-action README](https://github.com/DeterminateSystems/nix-installer-action)
- 官方 nix.dev 教程用 `cachix/install-nix-action@v25` + `cachix/cachix-action@v14` + secrets `CACHIX_SIGNING_KEY`/`CACHIX_AUTH_TOKEN`： [nix.dev: Continuous integration with GitHub Actions](https://nix.dev/guides/recipes/continuous-integration-github-actions)
- **安全警告（官方原文）**："Cachix tokens and signing keys provide full read and/or write access to your caches. GitHub Actions allows anyone who can edit workflow files to read secrets… Forked pull requests cannot access secrets, so they can only read from public caches." [cachix/cachix-action README](https://github.com/cachix/cachix-action)
- 反模式：不要对 fork PR 开 push（PR 分支上 `skipPush: true`，或仅 `push:` 事件推缓存）。同上。

### 3c. devenv 官方集成

**结论**：devenv 内置 Cachix 集成，`devenv.nix` 里三个选项：`cachix.pull`（列表，默认含 `devenv`）、`cachix.push`（字符串，指定后也会加入 pull）、`cachix.enable`（默认 `true`，`false` 关闭）。**`devenv.cachix.org` 默认加入 pull**——它镜像 devenv-nixpkgs/rolling（本项目 `devenv.yaml` 正是这个输入），这是 devenv 环境能秒级拉起的关键。认证：`CACHIX_AUTH_TOKEN` 环境变量（或 `cachix authtoken` 写入的 `~/.config/cachix/cachix.dhall`；devenv 2.2+ 还支持 SecretSpec + keyring）。CI 里"只有 CI 才推"的官方手法是写 `devenv.local.nix`。

```nix
# devenv.nix
{
  cachix.pull = [ "qingluan" ];      # 默认已经含 devenv
}
# 推（一般不放主配置，见下）
{
  cachix.push = "qingluan";
}
# 关闭集成
{
  cachix.enable = false;
}
```

```bash
# CI 里条件性开启推送（官方推荐：不写进主 devenv.nix）
$ echo '{ cachix.push = "qingluan"; }' > devenv.local.nix
$ export CACHIX_AUTH_TOKEN=XXX
$ devenv test   # 构建 shell 并自动把产物推入 qingluan
```

[Binary caching - devenv](https://devenv.sh/binary-caching/) · [src/modules/cachix.nix（选项定义源码）](https://github.com/cachix/devenv/blob/main/src/modules/cachix.nix) · [devenv.nix options 参考](https://devenv.sh/reference/options/)

- 关于"devenv ci 自动推缓存"：`devenv ci` 在 devenv 1.0 已改名为 **`devenv test`**（范围更广：构建 shell + 跑 git hooks）。 [devenv 1.0 发布博客](https://devenv.sh/blog/2024/03/20/devenv-10-rewrite-in-rust/) · [Using devenv in GitHub Actions（官方完整 workflow 示例）](https://devenv.sh/integrations/github-actions/)
- devenv 官方 GitHub Actions 示例：`install-nix-action@v31` → `cachix-action`（`name: devenv`）→ `nix profile add nixpkgs#devenv` → `devenv test`。 [Using devenv in GitHub Actions](https://devenv.sh/integrations/github-actions/)
- 坑：devenv 判定"你不是 trusted user"时给出两个选项——(a) 把自己加进 `trusted-users = root <user>` 并重启 nix-daemon，或 (b) 手动在 `/etc/nix/nix.conf` 配 substituters 并在 devenv.nix 里 `cachix.enable = false`。 [cachix/devenv issue #1604](https://github.com/cachix/devenv/issues/1604)

---

## 4. 下游消费路径

### 4a. flake.nix：`nixConfig.extra-substituters` + `extra-trusted-public-keys`

**结论**：在 flake 里声明项目专属 cache 的官方方式。Nix 手册：`nixConfig` 是 "an attribute set of values which reflect the values given to nix.conf… can extend the normal behavior of a user's nix experience by adding flake-specific configuration, such as a binary cache"。安全设计：**`nixConfig` 只能改 nix.conf 里的一小组选项且默认需要用户交互确认**（`accept-flake-config` 默认 `false`）——首次构建下游 flake 时 Nix 会逐个询问 "do you want to allow configuration setting 'extra-substituters' to be set to 'https://qingluan.cachix.org' (y/N)?"，接受后记录到 `~/.local/share/nix/trusted-settings.json`（按用户记录，刻意不入库，防止仓库作者借 flake 诱导用户信任恶意 cache）。

```nix
# qingluan/flake.nix（下游消费者无需做任何事，构建时会提示一次）
{
  nixConfig = {
    extra-substituters = [
      "https://qingluan.cachix.org"
      "https://devenv.cachix.org"
    ];
    extra-trusted-public-keys = [
      "qingluan.cachix.org-1:XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX="
      "devenv.cachix.org-1:w1cLUi8dv3hnoSPGAuibQv+f9TZLr6cv/Hm9XgU50cw="
    ];
  };
}
```

- 与 `trusted-users` 的关系：非 trusted user 通过 `nixConfig` 指定 substituters 会被交互确认；要免提示可在系统级设置 `accept-flake-config = true`（或命令行 `--accept-flake-config`），但官方建议保持 `false`——"Keep this set to false, as automatically accepting those options—without the prompt above—is more insecure than you think"。 [Nix manual: nix.conf (accept-flake-config)](https://nix.dev/manual/nix/stable/command-ref/conf-file#conf-accept-flake-config) · [NixOS/nix issue #7086](https://github.com/NixOS/nix/issues/7086) · [NixOS Discourse: interactive flake settings](https://discourse.nixos.org/t/interactive-flake-settings-nixconfig/76721)
- `extra-` 前缀语义：对列表型配置项，`extra-xxx` 追加到已有值之后（"substituters = a b; extra-substituters = c d" ⇒ `a b c d`）。 [Nix manual: nix.conf](https://nix.dev/manual/nix/stable/command-ref/conf-file)
- 已知问题：历史上 `nix run` 不应用 flake 的 `nixConfig`（`applyNixConfig` 默认 false，[NixOS/nix#6170](https://github.com/NixOS/nix/issues/6170)），且 `extra-*` 在部分场景会被忽略（[NixOS/nix#9487](https://github.com/NixOS/nix/issues/9487)）——所以对外宣传时给用户"系统级配置"和"命令行参数"两种兜底，见 4b/4c。

### 4b. NixOS：`nix.settings.substituters` / `trusted-public-keys`（系统级）

**结论**：NixOS 用户声明式配置（对 flake 引用者、`nixos-rebuild` 场景都生效，且不受 `accept-flake-config` 提示影响）：

```nix
{ ... }: {
  nix.settings = {
    substituters = [
      "https://qingluan.cachix.org?priority=40"
      "https://devenv.cachix.org"
      "https://cache.nixos.org"
    ];
    trusted-public-keys = [
      "qingluan.cachix.org-1:XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX="
      "devenv.cachix.org-1:w1cLUi8dv3hnoSPGAuibQv+f9TZLr6cv/Hm9XgU50cw="
      "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY="
    ];
  };
}
```

[nix.dev: Configure Nix to use a custom binary cache](https://nix.dev/guides/recipes/add-binary-cache)（NixOS 模块写法）· [NixOS Wiki: Binary Cache](https://wiki.nixos.org/wiki/Binary_Cache)（含非 NixOS 的 `/etc/nix/nix.conf` 写法）

- 非 NixOS 用户（含 WSL 多用户）同样可在 `/etc/nix/nix.conf` 或（trusted user）`~/.config/nix/nix.conf` 写 `substituters` / `trusted-public-keys`。 [NixOS Wiki: Binary Cache](https://wiki.nixos.org/wiki/Binary_Cache)
- NixOS 已知限制：配置后**第一次** `nixos-rebuild switch` 不会用上新 cache，第二次才生效（[cachix/cachix issue #323](https://github.com/cachix/cachix/issues/323)，FAQ 也收录）。 [Cachix FAQ](https://docs.cachix.org/faq)

### 4c. 临时消费（nix run / 命令行）

**结论**：不落盘配置的一次性用法——用 `--option` 传参（对 `nix run` 尤其重要，因为历史版本 `nix run` 不读 flake 的 `nixConfig`）：

```bash
$ nix run github:qingluan/qingluan \
    --option extra-substituters "https://qingluan.cachix.org" \
    --option extra-trusted-public-keys "qingluan.cachix.org-1:XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX="
```

[NixOS/nix issue #6170（nix run 不应用 nixConfig，命令行参数总是有效）](https://github.com/NixOS/nix/issues/6170) · [NixOS Wiki: Binary Cache（`nix-build --option substituters … --option trusted-public-keys …` 形式）](https://wiki.nixos.org/wiki/Binary_Cache)

---

## 5. 密钥管理细节：public key 格式、轮换、丢失后果

**结论**：public key 格式为 `<cache-name>-<key-id>:<base64>`（例如 `devenv.cachix.org-1:w1cLUi8dv3hnoSPGAuibQv+f9TZLr6cv/Hm9XgU50cw=`）。key-id 后缀递增即轮换标记（如 `spectrum-os.org-2:…` 取代 `-1:…`）。Cachix 目前**不支持为已有 cache 重新生成/追加签名密钥**；self-signed 模式下私钥只有本地一份，丢失后无法再 push 新路径（旧条目不受影响），只能重建 cache。managed 模式无此风险（Cachix 托管私钥），这也是官方推荐默认 managed 的原因。

- 格式：NixOS Wiki 示例即 `nix-community.cachix.org-1:mB9FSh9qf2dCimDSUo8Zy7bkq5CX+/rkCWyvRCYg3Fs=`；轮换示例 `spectrum-os.org-2:foQk3r7t2VpRx92CaXb5ROyy/NBdRJQG2uX2XJMYZfU=`（旧 `-1:` 与新 `-2:` 并存过渡）。 [NixOS Wiki: Binary Cache](https://wiki.nixos.org/wiki/Binary_Cache) · [Spectrum: Binary cache key rotation](https://inbox.spectrum-os.org/spectrum-devel/87zfpc5m2t.fsf@alyssa.is/T/)
- 不能随便重新生成：官方维护者确认 "Public signing key already exists. **It's currently not possible to override or add multiple signing keys.** However, this feature is planned."（issue #292，用户换机器丢了私钥后只能找回旧备份才能继续 push）。 [cachix/cachix issue #292](https://github.com/cachix/cachix/issues/292)
- 丢失 signing key 的后果：self-signed cache 无法再 push（Nix 会拒绝签名不匹配的产物）；官方给出的缓解是**重建 cache**："You can recreate the private cache for now in case signing key is leaked."（多签名/轮换在 todo 上）。 [cachix/cachix issue #146（Key Rotation?）](https://github.com/cachix/cachix/issues/146)
- 私钥唯一副本：`generate-keypair` 输出 "IMPORTANT: Make sure to make a backup for the signing key above, as you have the only copy."；换机器需要把私钥手工加入 `~/.config/cachix/cachix.dhall` 或 `export CACHIX_SIGNING_KEY`。 [cachix/cachix issue #71](https://github.com/cachix/cachix/issues/71)
- push 不覆盖：同一 store path 已存在时不会覆盖，"The existing entry first needs to be deleted."（这与"密钥无法轮换"共同意味着：宁可一开始就用 managed cache）。 [Cachix FAQ](https://docs.cachix.org/faq)

---

## 6. 定价/限制

**结论**：开源项目免费（5 GB 压缩存储 + 20 个 Cachix Deploy agent）；带宽无限制（Cloudflare CDN）。所有条目压缩存储（"saves up to 90% of storage"），且**推送前先检查 cache.nixos.org 上游，已存在的条目不重复占用存储**。存储达到上限后按"最近最少使用（LRU）"自动删除最旧条目；达到 85% 时发警告邮件。付费档：Starter 50 GB / Standard 250 GB / Pro 1500 GB，所有计划 14 天试用。

- "Users have a free 5 GB limit for open source projects and 20 Cachix Deploy agents." [Pricing - cachix.org](https://www.cachix.org/pricing)
- "All entries are compressed, which saves up to 90% of storage. By default entries in cache.nixos.org are never stored in Cachix to save space." [Pricing - cachix.org](https://www.cachix.org/pricing)
- 组织同样享受 5 GB 免费计划（2023-09-12 存储计划升级公告）。 [Cachix Blog: Upgraded storage plans](https://blog.cachix.org/posts/2023-09-12-upgraded-storage-plans/)
- GC 规则："Once you reach 85% of your storage limit, you will receive a warning email… Garbage collection algorithm sorts all store paths by their last accessed date… deletes the oldest entries up until your storage limit." [Garbage Collection - Cachix docs](https://docs.cachix.org/garbage-collection)
- 私库需要付费计划；免费计划仅公共 cache（开源）。 [Pricing - cachix.org](https://www.cachix.org/pricing)

---

## 7. 常见坑

1. **Cachix 侧 GC / retention**：免费 5 GB 上限后 LRU 自动删最旧条目；关键产物用 `cachix pin <cache> <name> <path> --keep-days 3`（或 `--keep-revisions N`）保住（pin 默认免疫 GC）。 [Garbage Collection - Cachix docs](https://docs.cachix.org/garbage-collection) · [Pins - Cachix docs](https://docs.cachix.org/pins)
2. **本地 Nix GC 与 push 竞态**：`InvalidPath` 错误通常是 push 过程中本地 GC 删掉了路径，或路径根本没构建完；确认 GC 时间戳与报错时间是否吻合。 [Cachix FAQ](https://docs.cachix.org/faq)
3. **CI token 泄漏**：token/signing key 拥有 cache 完全读写权；GitHub Actions 里**任何能编辑 workflow 文件的人都能读 secrets**；恶意 fork 代码合并进主分支可窃取 token（"Malicious code merged from forks can reveal your tokens"）。对策：per-cache token 而非 personal token、fork PR 不推、定期轮换；历史安全公告（HTTP 错误日志泄漏 auth token，GHSA-5v3x-gf4h-9qrh）已修复，但提示了 token 敏感度。 [cachix/cachix-action README](https://github.com/cachix/cachix-action) · [GHSA-5v3x-gf4h-9qrh advisory](https://github.com/cachix/cachix/security/advisories/GHSA-5v3x-gf4h-9qrh)
4. **trusted-users 是 pull 侧的前提，push 侧需要的是 token**：`cachix use` / devenv 自动配置 substituters 需要用户是 trusted user（否则只能手工写 `/etc/nix/nix.conf` 并 `cachix.enable = false`）；而 `cachix push` 只需要 auth token（写权限）。WSL/单用户 Nix 上把用户加进 `trusted-users = root <user>` 即可。 [Cachix FAQ](https://docs.cachix.org/faq) · [devenv issue #1604](https://github.com/cachix/devenv/issues/1604)
5. **cache 是全局扁平命名空间，不是按 commit**：cache 内容以 store path 哈希寻址、跨 commit/分支/机器去重共享；不存在"某 commit 的缓存"概念。`devenv-nixpkgs/rolling` 每日滚动 → 闭包哈希每天变化 → 每天会有新构建（旧哈希仍命中缓存）。需要按版本保留时用 pin。 [Garbage Collection - Cachix docs](https://docs.cachix.org/garbage-collection) · [devenv issue #1709（自定义 nixpkgs 导致 CI 重新编译，cache 未命中的实例）](https://github.com/cachix/devenv/issues/1709)
6. **负缓存 / 时序**：缓存填充前跑过 `nix-build` 会产生负缓存（Nix 不再重新检查）；NixOS 首次 rebuild 不生效（见 4b）。 [Cachix FAQ](https://docs.cachix.org/faq)
7. **推送不覆盖**：同名 store path 已存在时不覆盖，需先删除（见第 5 节）。 [Cachix FAQ](https://docs.cachix.org/faq)
8. **不要把 cache.nixos.org 已有的东西推进去**：Cachix 自动跳过上游已有条目，但自建 CI 若 push 整个 store 会浪费带宽，用 `nix build --no-link --print-out-paths` 只推目标闭包即可。 [Garbage Collection - Cachix docs](https://docs.cachix.org/garbage-collection) · [Pushing to Cachix - Cachix docs](https://docs.cachix.org/pushing)

---

## 8. 针对本仓库（qingluan）的落地建议

现状盘点：
- `devenv.yaml`：`inputs.nixpkgs.url = github:cachix/devenv-nixpkgs/rolling`（**不在 cache.nixos.org 上**，CI/新机器必须从 `devenv.cachix.org` 拉，否则从源码构建 stdenv）。
- `devenv.nix`：rust + pnpm + cargo-tauri，未配置任何 cachix 选项（默认只拉 `devenv` cache）。
- 仓库根**没有 `flake.nix`**（上份报告建议补一个，用 `devenv.lib.mkShell` + `packages.qingluan`）。
- `.github/workflows/quality.yml`：裸 rust(bun) 工具链，未用 Nix。

### 落地步骤

1. **建 cache**：app.cachix.org 建公共 cache `qingluan`（默认 managed 签名，**不要**选 self-signed，避免第 5 节的所有密钥保管负担）。记下页面给的 public key（形如 `qingluan.cachix.org-1:…=`）与 push 命令。
2. **Cache Settings 生成 per-cache token** → GitHub repo 加 secret `CACHIX_AUTH_TOKEN`（不要用 personal token）。
3. **flake.nix**（结合上份报告补的 flake）加：

```nix
{
  nixConfig = {
    extra-substituters = [ "https://qingluan.cachix.org" "https://devenv.cachix.org" ];
    extra-trusted-public-keys = [
      "qingluan.cachix.org-1:XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX="
      "devenv.cachix.org-1:w1cLUi8dv3hnoSPGAuibQv+f9TZLr6cv/Hm9XgU50cw="
    ];
  };
}
```

4. **devenv.nix** 加 `cachix.pull = [ "qingluan" ];`（默认 `devenv` 仍在列表里）。
5. **本地推**（可选）：`nix build --no-link --print-out-paths .#packages.x86_64-linux.qingluan | cachix push qingluan`；或 `cachix watch-exec qingluan -- devenv test`。
6. **CI**：改造 `quality.yml`——保留原 rust/bun job（不折腾），**新增一个 nix job** 负责构建 devShell/包并推 cache：

```yaml
# .github/workflows/quality.yml（在现有 quality job 之外新增；也可单独建 cachix.yml）
jobs:
  quality:
    # ...原有裸 rust+bun job 保持不变...

  nix:
    runs-on: ubuntu-latest
    # fork PR 无 secrets，天然只读；push 到 main 才允许写缓存
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@v4

      # 方案 A（devenv 官方同款组合）：
      - uses: cachix/install-nix-action@v31
        with:
          nix_path: nixpkgs=channel:nixos-unstable
      # 方案 B（等价替换）：DeterminateSystems/nix-installer-action@main
      #   （自动启用 flakes、auto-optimise-store；需在 cachix-action 之前运行）

      - uses: cachix/cachix-action@v17
        with:
          name: qingluan
          authToken: "${{ secrets.CACHIX_AUTH_TOKEN }}"   # managed cache 只需 token
          # signingKey: "${{ secrets.CACHIX_SIGNING_KEY }}" # 仅 self-signed 时需要
          skipPush: ${{ github.event_name == 'pull_request' }}
          extraPullNames: devenv   # 拉 devenv.cachix.org，覆盖 devenv-nixpkgs/rolling

      - name: Install devenv
        run: nix profile add nixpkgs#devenv

      # 只在自己的 main 上开推（官方推荐 devenv.local.nix 手法）
      - name: Enable Cachix push
        if: github.event_name == 'push' && github.ref == 'refs/heads/main'
        run: echo '{ cachix.push = "qingluan"; }' > devenv.local.nix

      - name: Build devenv shell and push to cache
        run: devenv test   # 构建 shell + 跑 git hooks；cachix.push 生效时自动上传产物

      # 可选：打包产物也进缓存（上份报告的 packages.qingluan）
      - name: Build and push package
        if: github.event_name == 'push' && github.ref == 'refs/heads/main'
        run: |
          nix build --no-link --print-out-paths .#packages.x86_64-linux.qingluan \
            | cachix push qingluan
```

要点：
- `skipPush: ${{ github.event_name == 'pull_request' }}`：PR 只拉不推；push 到 main 才推。secrets 对 fork PR 本就不可见（官方 README 明确"Forked pull requests cannot access secrets"），双保险。
- `extraPullNames: devenv` 让 CI 从 `devenv.cachix.org` 拉 devenv-nixpkgs/rolling 的预编译产物——**没有它 CI 会从源码构建 stdenv**（dev shell 首次构建几十分钟）。
- devenv 的 `cachix.push` 会把该 cache 自动加入 pull 列表（`src/modules/cachix.nix` 源码：`cachix.pull = [ "devenv" ] ++ (lib.optional (cfg.push != null) config.cachix.push)`）。 [src/modules/cachix.nix](https://github.com/cachix/devenv/blob/main/src/modules/cachix.nix)
- 成本预估：免费 5 GB 压缩存储。devShell 闭包大部分条目在 cache.nixos.org/devenv.cachix.org 已有（不重复存储），实际新增只有 cargo 依赖、Tauri 相关 patch 产物与最终包，量级远低于 5 GB。

---

## Sources

**Kept（一手来源）**
- [What is a Binary Cache? - Cachix docs](https://docs.cachix.org/what-is-a-binary-cache) — 概念：push 构建产物 / 下游 substituter 拉取
- [Getting Started - Cachix docs](https://docs.cachix.org/getting-started) — 建 cache、token 类型、public key、generate-keypair、cachix use
- [Pushing to Cachix - Cachix docs](https://docs.cachix.org/pushing) — 本地/CI 全部官方 push 命令
- [Security - Cachix docs](https://docs.cachix.org/security) — access token / signing key / managed vs self-signed
- [FAQ - Cachix docs](https://docs.cachix.org/faq) — cachix use 的配置落点、push 不覆盖、InvalidPath、负缓存、NixOS 首次 rebuild 限制、cachix doctor
- [Garbage Collection - Cachix docs](https://docs.cachix.org/garbage-collection) — 上游去重、85% 警告、LRU 删除算法
- [Pins - Cachix docs](https://docs.cachix.org/pins) — pin 保留策略（--keep-days/--keep-revisions）
- [Pricing - cachix.org](https://www.cachix.org/pricing) — 免费 5 GB / 压缩存储 / 付费档位
- [Cachix Blog: Upgraded storage plans (2023-09-12)](https://blog.cachix.org/posts/2023-09-12-upgraded-storage-plans/) — 组织免费 5 GB 计划
- [cachix/cachix-action README + action.yml](https://github.com/cachix/cachix-action) — 全部 inputs（name/authToken/signingKey/skipPush/useDaemon/extraPullNames）、secrets 安全模型
- [DeterminateSystems/nix-installer-action README](https://github.com/DeterminateSystems/nix-installer-action) — 安装 Nix（flakes、auto-optimise-store）+ 与 cachix-action 组合方式
- [Binary caching - devenv](https://devenv.sh/binary-caching/) — cachix.pull / push / enable、devenv.cachix.org 默认、devenv.local.nix、SecretSpec
- [src/modules/cachix.nix（devenv 源码）](https://github.com/cachix/devenv/blob/main/src/modules/cachix.nix) — 选项定义与默认行为
- [Using devenv in GitHub Actions - devenv](https://devenv.sh/integrations/github-actions/) — 官方完整 CI workflow（install-nix-action + cachix-action + devenv test）
- [devenv 1.0 博客（devenv ci → devenv test）](https://devenv.sh/blog/2024/03/20/devenv-10-rewrite-in-rust/)
- [Nix manual: nix.conf](https://nix.dev/manual/nix/stable/command-ref/conf-file) — substituters / trusted-users / trusted-substituters / accept-flake-config / extra- 前缀
- [nix.dev: Configure Nix to use a custom binary cache](https://nix.dev/guides/recipes/add-binary-cache) — 信任模型警告 + NixOS 模块写法
- [nix.dev: Continuous integration with GitHub Actions](https://nix.dev/guides/recipes/continuous-integration-github-actions) — cachix-action 官方 CI 用法
- [NixOS Wiki: Binary Cache](https://wiki.nixos.org/wiki/Binary_Cache) — public key 格式、/etc/nix/nix.conf、命令行 --option
- [cachix/cachix issue #292](https://github.com/cachix/cachix/issues/292) — 不能覆盖/追加签名密钥
- [cachix/cachix issue #146](https://github.com/cachix/cachix/issues/146) — key 轮换规划、泄密后重建 cache
- [cachix/cachix issue #71](https://github.com/cachix/cachix/issues/71) — signing key 唯一副本、CACHIX_SIGNING_KEY
- [GHSA-5v3x-gf4h-9qrh（token 泄漏 advisory）](https://github.com/cachix/cachix/security/advisories/GHSA-5v3x-gf4h-9qrh) — token 敏感性
- [NixOS/nix issue #6170](https://github.com/NixOS/nix/issues/6170) — nix run 不应用 flake nixConfig
- [NixOS/nix issue #7086](https://github.com/NixOS/nix/issues/7086) — --accept-flake-config
- [NixOS Discourse: interactive flake settings](https://discourse.nixos.org/t/interactive-flake-settings-nixconfig/76721) — trusted-settings.json 按用户记录
- [Spectrum: Binary cache key rotation](https://inbox.spectrum-os.org/spectrum-devel/87zfpc5m2t.fsf@alyssa.is/T/) — -1/-2 后缀轮换实例
- [cachix/devenv issue #1604](https://github.com/cachix/devenv/issues/1604) — trusted-user 报错与两种解决路径
- [cachix/devenv issue #1709](https://github.com/cachix/devenv/issues/1709) — 自定义 nixpkgs 时 cache 未命中的实际案例

**Dropped**
- [nixos-and-flakes-book](https://nixos-and-flakes.thiscute.world/nix-store/add-binary-cache-servers) — 中文二手中转，内容与 Nix 手册一致但以手册为准；仅作参考
- [Garnix blog: Stop trusting Nix caches](https://garnix.io/blog/stop-trusting-nix-caches/) — 第三方观点文（信任模型背景），非一手操作文档
- [ethancedwards.com 博客](https://ethancedwards.com/blog/building-nix-with-gha) — 个人实践，非官方
- [cran.r-project.org rix 教程](https://cran.r-project.org/web/packages/rix/vignettes/binary-cache.html) — 第三方教程，仅印证免费档 5GB
- [omegon docs/design/cachix-binary-cache.md](https://github.com/styrene-lab/omegon/blob/main/docs/design/cachix-binary-cache.md) — 第三方设计文档
- [NixOS/rfc 149、Nix PR #12976](https://github.com/NixOS/rfcs/pull/149) — cache.nixos.org 官方 key 轮换 RFC，与 Cachix 服务本身无直接关系

## Gaps

- **定价页精确数值**：`cachix.org/pricing` 页面抓取失败，免费档 5 GB / 带宽无限制 / 付费档容量来自搜索索引摘要与官方博客公告（2023-09-12），建议接入前在页面上复核最新档位。
- **managed cache 的轮换**：Cachix 对 managed cache 的私钥轮换策略（服务端行为）无公开文档细节；对用户而言表现为 public key 不变。
- **`nix run` 应用 nixConfig 的修复版本**：issue #6170 的修复随版本演进，未逐版验证；对外文档仍建议命令行 `--option` 兜底。
- **devenv 2.2 SecretSpec**（keyring 存 token）为新特性，qingluan 团队如不想用 keyring 可走传统 `CACHIX_AUTH_TOKEN` 路径，两者官方都支持。

## 验收说明（review-findings / residual-risks）

- review-findings：无 blocker。所有结论均锚定一手来源链接；命令块照抄官方文档（`cachix-action@v17` 为当前 README 版本号，devenv 官方示例仍写 `v16`，版本以 GitHub Marketplace 最新 tag 为准）。
- residual-risks：(1) 定价页具体数字未直接抓取（见 Gaps）；(2) public key 占位符 `XXXXXXXXXXXXXXXX…` 需在真实创建 cache 后替换；(3) 本仓库尚无 `flake.nix`，`nixConfig`/`packages.qingluan` 片段依赖上份报告的 flake 落地后生效；(4) `extraPullNames: devenv` 在 CI 中为必需项，遗漏会导致 devenv-nixpkgs/rolling 从源码构建。
