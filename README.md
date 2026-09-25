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
  enable = true;          # 安装 qingluan CLI + daemon
  daemon.enable = true;   # 可选：启动 Delegate=yes 的 systemd user unit
  desktop.enable = true;  # 可选：Tauri 桌面端
};
```

NixOS 用 `nixosModules.qingluan`，选项相同。临时体验：`nix run github:sikongjueluo/qingluan`。

## 使用

```bash
systemctl --user start qingluan-daemon  # 推荐：带 cgroup delegation 启动
qingluan daemon start       # 手动启动；当前 scope 必须已获 cgroup delegation
# 可用 --host 0.0.0.0 监听局域网 HTTP；terminal gRPC 仍仅使用私有 UDS
qingluan health            # 健康检查
qingluan workspace list    # workspace 列表
qingluan workspace add foo # 新建 workspace（默认 ~/Projects/.workspace/<repo>/foo）
qingluan workspace remove foo [--purge]  # 移除（--purge 连目录删）
qingluan-desktop           # 桌面端
```

配置见 [`config/qingluan.example.toml`](config/qingluan.example.toml)。四层合并
（内置默认 < `~/.config/qingluan/config.toml` < 项目根 `qingluan.toml` < `QINGLUAN_*` 环境变量），
CLI 可用 `--daemon-url` 覆盖；配置文件损坏时硬失败不静默降级。

## 开发

```bash
devenv shell      # 开发环境（rust, pnpm, bun, cargo-tauri, cargo-watch）
just dev          # daemon(watch) + 前端并行
just tauri-dev    # 桌面端开发模式
just quality      # 质量门（CI 同款）
just terminal-grpc-interop # tonic UDS ↔ grpc-js 协议互操作门
nix build .#qingluan-desktop   # Nix 打包
```

目录：`crates/`（Rust）、`apps/desktop/`（Tauri）、`nix/`（打包与模块）、`docs/`（文档）。

## License

AGPL-3.0-or-later
