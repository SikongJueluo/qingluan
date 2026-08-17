# Qingluan 青鸾

Agent 任务平台：Rust daemon + CLI + Tauri 2 桌面端。

## 安装（NixOS / home-manager）

```nix
inputs.qingluan.url = "github:sikongjueluo/qingluan";
```

```nix
# home-manager
imports = [ inputs.qingluan.homeManagerModules.qingluan ];
programs.qingluan = {
  enable = true;          # qingluan CLI + daemon
  desktop.enable = true;  # 可选：Tauri 桌面端
};
```

NixOS 用 `nixosModules.qingluan`，选项相同。临时体验：`nix run github:sikongjueluo/qingluan`。

## 使用

```bash
qingluan-daemon            # 启动 daemon（127.0.0.1:47129）
qingluan health            # 健康检查
qingluan workspace list    # workspace 列表
qingluan-desktop           # 桌面端
```

配置见 [`config/qingluan.example.toml`](config/qingluan.example.toml)，CLI 可用 `--daemon-url` 覆盖。

## 开发

```bash
devenv shell      # 开发环境（rust, pnpm, bun, cargo-tauri, cargo-watch）
just dev          # daemon(watch) + 前端并行
just tauri-dev    # 桌面端开发模式
just quality      # 质量门（CI 同款）
nix build .#qingluan-desktop   # Nix 打包
```

目录：`crates/`（Rust）、`apps/desktop/`（Tauri）、`nix/`（打包与模块）、`docs/`（文档）。

## License

AGPL-3.0-or-later
