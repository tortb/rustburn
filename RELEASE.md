# rustburn 构建与发布操作手册

## 概述

rustburn 通过 GitHub Actions 实现自动化跨平台构建与发布。每次推送 `v*` 格式的 tag 时，workflow 会在 5 个平台上并行构建，生成 Release 资产，并自动创建 GitHub Release。

## 发布架构

```
git tag v0.2.0 && git push origin v0.2.0
           │
           ▼
  .github/workflows/release.yml
           │
     ┌─────┼─────┬──────────────┬──────────────┐
     ▼     ▼     ▼              ▼              ▼
  ubuntu ubuntu macos          macos          windows
  x86_64 aarch64 aarch64       x86_64         x86_64
  (cargo) (cross) (cargo)      (cargo)        (cargo)
     │     │     │              │              │
     ▼     ▼     ▼              ▼              ▼
  tar.gz tar.gz tar.gz         tar.gz         zip
     └─────┴─────┴──────────────┴──────────────┘
                       │
                       ▼
              gh release create
           (5 平台资产 + SHA256SUMS)
                       │
                       ▼
              install.sh 动态获取最新版本
```

## 支持的平台与目标三元组

| 平台 | 目标三元组 | 构建方式 | 产物格式 |
| --- | --- | --- | --- |
| Linux x86_64 | `x86_64-unknown-linux-gnu` | 本地 `cargo` | `.tar.gz` |
| Linux aarch64 | `aarch64-unknown-linux-gnu` | `cross` | `.tar.gz` |
| macOS Apple Silicon | `aarch64-apple-darwin` | 本地 `cargo` | `.tar.gz` |
| macOS Intel | `x86_64-apple-darwin` | 本地 `cargo`（macOS 上交叉编译） | `.tar.gz` |
| Windows x86_64 | `x86_64-pc-windows-msvc` | 本地 `cargo` | `.zip` |

## 发布流程

### 方式一：GitHub Actions 自动发布（推荐）

**适用场景**：正式发布完整版本，需要全平台构建。

**步骤**：

1. 确保所有改动已合并到 `master` 分支，且 CI 通过：

   ```sh
   cargo build --workspace
   cargo test --workspace
   cargo clippy --workspace --all-targets --all-features -- -D warnings
   cargo fmt --check
   ```

2. 在本机更新 `Cargo.toml` 中的版本号（如 `0.1.0` → `0.2.0`）：

   ```sh
   # 编辑 Cargo.toml 第 10 行 version 字段
   ```

3. 提交版本号变更并推送：

   ```sh
   git add Cargo.toml
   git commit -m "chore: bump version to 0.2.0"
   git push origin master
   ```

4. 创建 tag 并推送，触发 Actions 构建：

   ```sh
   git tag v0.2.0
   git push origin v0.2.0
   ```

5. 等待 Actions 完成（约 3-5 分钟），可在以下地址查看进度：

   ```sh
   gh run list --workflow=release.yml --limit 1
   gh run watch <RUN_ID>
   ```

6. 检查 Release 资产完整性：

   ```sh
   gh release view v0.2.0 --json assets --jq '.assets[].name'
   ```

   预期输出 5 个二进制压缩包 + 1 个 `SHA256SUMS`。

### 方式二：本地手动构建

**适用场景**：快速发布仅含当前主机平台的单平台资产，或测试构建流程。

**步骤**：

1. 构建当前主机平台（自动检测目标三元组）：

   ```sh
   ./build-release.sh
   ```

   产物输出到 `target/release-artifacts/`。

2. 指定目标平台（逗号分隔）：

   ```sh
   ./build-release.sh v0.2.0 --targets x86_64-unknown-linux-gnu,aarch64-unknown-linux-gnu
   ```

   **注意**：跨平台编译需要安装对应目标工具链或 `cross`。当前主机无 Docker 时 `cross` 会回退到本地 `cargo`，仅能构建主机平台。

3. 手动发布：

   ```sh
   gh release create v0.2.0 target/release-artifacts/* \
     --title "v0.2.0" \
     --generate-release-notes
   ```

## 安装脚本原理

[install.sh](install.sh) 是纯 POSIX sh 脚本，不依赖 jq / wget / rust / git。

**执行流程**：

```
curl -fsSL install.sh | sh
         │
         ▼
  1. 检测平台 (uname -s / uname -m)
         │
         ▼
  2. 请求 GitHub API /releases/latest → 获取 tag_name
         │
         ▼
  3. 构造下载 URL，校验 release 中是否包含该 asset
         │
         ▼
  4. 下载 tar.gz/zip 到临时目录
         │
         ▼
  5. SHA256 强制校验（失败即退出，无跳过开关）
         │
         ▼
  6. 解压 → 找到 rb 二进制
         │
         ▼
  7. 原子替换到 ~/.local/bin/rb（先 cp 到临时文件 → 校验 → mv）
         │
         ▼
  8. 检查 PATH，不在则提示用户手动添加
         │
         ▼
  9. trap 清理临时文件
```

**安全特性**：

- 仅访问 `api.github.com` / `github.com` / `objects.githubusercontent.com`
- 无遥测、无数据上传、不修改 shell 配置
- 不需要 sudo，安装在 `~/.local/bin/`
- 下载/校验/解压任一环节失败，已安装的旧 `rb` 保持不变

## 构建依赖说明

### git2 配置

[Cargo.toml](Cargo.toml) 中 `git2` 关闭了默认特性：

```toml
git2 = { version = "0.19", default-features = false }
```

默认特性启用 `ssh` 和 `https`，会引入 `openssl-sys` 和 `libssh2-sys` 系统依赖，导致 Windows MSVC 和 macOS 上无法直接构建。rustburn 仅使用 `git2` 的本地仓库分析功能（revwalk、diff），不需要远程传输，关闭默认特性不影响功能。

### 本地系统依赖

仅 Linux 本地构建需要 `zlib-devel`（`apt install zlib1g-dev`），`libgit2-sys` 构建时依赖。GitHub Actions ubuntu runner 已预装。

## 发布检查清单

每次发布前确认：

- [ ] `cargo test --workspace` 全量通过
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings` 无警告
- [ ] `cargo fmt --check` 格式正确
- [ ] `Cargo.toml` 版本号已更新
- [ ] `install.sh` 安装脚本在目标平台可正常执行
- [ ] `build-release.sh` 无报错
- [ ] 提交并推送版本号变更
- [ ] 创建 tag 并推送触发 Actions
- [ ] 等待 Actions 完成，确认 5 个 job 全部 success
- [ ] 检查 Release 页面资产数量（5 个二进制 + 1 个 SHA256SUMS = 6 个文件）
- [ ] 验证 `curl -fsSL https://raw.githubusercontent.com/tortb/rustburn/master/install.sh | sh` 可正常安装

## 常见问题

### Q: `rb: command not found` 安装后无法使用？

A: install.sh 不会自动修改 shell 配置。需要手动将 `~/.local/bin` 加入 PATH：

```sh
# 在 ~/.zshrc 或 ~/.bashrc 中添加：
export PATH="$HOME/.local/bin:$PATH"
```

### Q: Actions 构建失败，如何排查？

A: 查看具体 job 日志：

```sh
gh run view <RUN_ID> --log --job <JOB_ID>
```

### Q: 如何只发布单平台资产？

A: 使用 `build-release.sh` 本地构建，再通过 `gh release create` 手动发布：

```sh
./build-release.sh
gh release create v0.2.0 target/release-artifacts/* --title "v0.2.0"
```

### Q: install.sh 校验失败？

A: 检查 Release 上的 `SHA256SUMS` 是否与资产匹配。如果手动上传了资产但未更新 `SHA256SUMS`，会导致校验失败。

## 文件清单

| 文件 | 用途 |
| --- | --- |
| `build-release.sh` | 本地打包脚本，产出 tar.gz/zip + SHA256SUMS |
| `.github/workflows/release.yml` | GitHub Actions 自动构建与发布 workflow |
| `install.sh` | 一键安装脚本，用户通过 `curl \| sh` 执行 |
| `Cargo.toml` | workspace 依赖与版本号 |
| `README.md` / `README.en.md` | 用户文档，含安装说明