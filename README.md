# rustburn

[English](README.en.md) | 中文

一条命令分析代码仓库中的技术债与潜在风险，生成自包含的单 HTML 报告。

- 零外部依赖：报告不依赖 CDN / 外部 JS / 外部字体，可离线打开、截图传播
- 评分透明：三层公式的所有中间值（维度综合值 / 百分位 / 基础风险分 / 趋势系数）均可追溯验证
- 隐私友好：不上传源码，唯一的网络请求是 OSV 依赖漏洞查询，可完全关闭

## 安装

### 从源码构建

```sh
git clone https://github.com/tortb/rustburn.git
cd rustburn
cargo build --release
# 二进制位于 target/release/rb，可复制到 PATH 中的任意目录
install -m 755 target/release/rb ~/.local/bin/rb
```

> 一键安装脚本（`curl -fs | sh`，含 SHA256 校验）随 GitHub Release 发布。

## 快速开始

```sh
rb                 # 扫描当前目录，生成 rustburn-report.html
rb scan ./project  # 扫描指定目录
rb --json          # 输出 JSON 报告（默认 rustburn-report.json）
rb --offline       # 离线模式（禁止任何网络请求）
rb --fail-above 70 # 分数超过 70 时退出码 1（可用于 CI）
rb --ignore target # 临时排除路径（可重复）
```

### 参数

| 参数 | 说明 | 默认 |
| --- | --- | --- |
| `-o, --output <FILE>` | 输出文件路径 | `rustburn-report.html` / `rustburn-report.json` |
| `--json` | 输出 JSON 报告 | 否 |
| `--offline` | 禁止网络请求 | 否 |
| `--max-commits <N>` | 最多分析的 commit 数 | `5000` |
| `--ignore <PATTERN>` | 排除路径（可重复，与 `.rbignore` 合并） | 无 |
| `--fail-above <SCORE>` | 分数超过阈值时退出码 1 | 无 |
| `-h, --help` / `-V, --version` | 帮助 / 版本 | - |

### 退出码

| 退出码 | 含义 |
| --- | --- |
| `0` | 扫描成功 |
| `1` | 分数超过 `--fail-above` 阈值 |
| `2` | 执行错误（路径不存在 / 非 git 仓库 / 输出失败等） |

普通警告（AST 解析失败、依赖查询失败、历史截断）不会导致非 0 退出。

## 排除规则

项目根目录的 `.rbignore`（gitignore 风格）与 `--ignore` 参数合并生效：

```text
node_modules/
dist/
target/
*.min.js
*.generated.rs
```

rustburn 默认**不忽略**任何目录（包括 `.git/`、`target/`、`node_modules/`），需要排除时请主动写入 `.rbignore` 或使用 `--ignore`。

## 支持的语言

- Rust（`.rs`）
- JavaScript / JSX（`.js`、`.jsx`）

其他语言自动跳过，不会导致扫描失败。符号链接默认不跟随。

## HTML 报告内容

生成的 `rustburn-report.html` 为自包含单文件，包含：

- **总览**：仓库总热力值（conic 环形进度条）、文件数、LOC、Top 5% 占比、平均置信度
- **文件树热力图**：面积 = LOC，颜色 = 热力分数；超过 500 个文件时自动降级为 Top 100 + 折叠其余
- **Top 风险文件**：可展开卡片，展示三层公式中间值与百分位进度条
- **文件详情**：原始指标、百分位、趋势曲线（Canvas 手写，含 commit sha / 日期 / 分数 tooltip）、一致性检查、公式推导
- **依赖漏洞**：按严重度排序的 CVE / GHSA / OSV 发现
- **异常标记**：历史重写、临时复杂度下降等疑点

## 评分模型（三层公式）

1. **基础风险分** = `100 - cbrt((100 - C) × (100 - H) × (100 - D))`，其中 C / H / D 是复杂度、历史、依赖三个维度在仓库内的百分位排名（0-100，越高越差）
2. **趋势系数** = `1 - trend_delta × 0.3`（历史快照分数变化，范围 0.91-1.09）
3. **最终热力值** = 基础风险分 × 趋势系数（clamp 0-100）

仓库总分 = LOC 加权平均 + Top 5% 文件惩罚（`top_files_avg × 0.2`）。

分数越高，风险越大。一致性系数只用于置信度展示，不参与最终分数。

## 防刷分机制

百分位是仓库内**相对排名**：即使攻击者通过拆分函数 / 文件稀释绝对指标，只要全仓库同比例变化，相对排名与基础风险分基本不变（相关测试保证相对变化 ≤ 15%）。历史重写、空 commit 等行为也会被检测或标记为未知，不会直接降低分数。

## 已知限制

- **临时修改不可检测**：单次扫描无法证明"扫描前临时优化、扫描后恢复"的行为
- **Rename 检测依赖 libgit2 similarity**：未被判定为 rename 的 delete + add 不自行做内容相似度检测
- **趋势历史（v1）**：尚未实现历史 commit 的逐点重算，无有效历史时 `trend_coefficient` 取 1.0（报告标记"趋势数据不足"）
- **离线模式**：依赖漏洞维度标记为数据不完整（Unknown），不会当作 0 处理

## 隐私

rustburn 不上传源码、文件内容、commit message、作者邮箱或仓库路径。
唯一的网络请求是对 OSV API 的依赖漏洞查询（仅发送包名 / 生态 / 版本），可通过 `--offline` 完全关闭。

## 开发

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --check
```

Workspace 结构：

- `crates/rustburn-cli` — CLI 入口（二进制 `rb`）
- `crates/rustburn-core` — 复杂度 / Git 历史 / 依赖 / 评分算法
- `crates/rustburn-report` — HTML 报告渲染（askama 模板）

## 参考

- [spec.md](spec.md) — 项目开发规格文档
- [cli-spec.md](cli-spec.md) — CLI 功能规格
