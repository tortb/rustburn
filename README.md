# rustburn

[English](README.en.md) | 中文

一条命令分析代码仓库中的技术债与潜在风险，生成自包含的单 HTML 报告。

- 零外部依赖：报告不依赖 CDN / 外部 JS / 外部字体，可离线打开、截图传播
- 评分透明：三层公式的所有中间值（维度综合值 / 百分位 / 基础风险分 / 趋势系数）均可追溯验证
- 隐私友好：不上传源码，唯一的网络请求是 OSV 依赖漏洞查询，可完全关闭

## 安装

### 一键安装（GitHub Releases）

```sh
curl -fsSL https://rb.tor.hk/install.sh | sh
```

也可通过 GitHub 原始地址安装（内容相同）：

```sh
curl -fsSL https://raw.githubusercontent.com/tortb/rustburn/master/install.sh | sh
```

安装脚本动态获取最新 Release，强制进行 SHA256 校验后安装到 `~/.local/bin/rb`（无需 sudo）。下载、校验、解压任一环节失败都会终止，且不会破坏已有安装；`~/.local/bin` 不在 PATH 时会给出提示，不会自动修改 shell 配置。

### 从源码构建

```sh
git clone https://github.com/tortb/rustburn.git
cd rustburn
cargo build --release
# 二进制位于 target/release/rb，可复制到 PATH 中的任意目录
install -m 755 target/release/rb ~/.local/bin/rb
```

## 快速开始

```sh
rb                 # 扫描当前目录，生成 rustburn-report.html
rb scan ./project  # 扫描指定目录
rb --json          # 输出 JSON 报告（默认 rustburn-report.json）
rb --offline       # 离线模式（禁止任何网络请求，同时禁用更新检测）
rb --fail-above 70 # 分数超过 70 时退出码 1（可用于 CI）
rb --ignore target # 临时排除路径（可重复）
rb update          # 检查并更新到 GitHub 最新发布版本（需交互确认）
rb --version       # 版本号 + git commit 短哈希 + 构建日期
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

## 评分模型（四层公式）

rustburn 的评分完全透明，每一层公式与源码实现严格一致：

**1. 维度综合值**（0-100，由原始指标计算）

```
complexity_value = 圈复杂度×0.4 + 嵌套深度×15×0.4 + 平均函数长度×0.2
history_value    = 归一化commit数×0.35 + 归一化作者数×0.10 + 归一化incident数×0.45 + 陈旧度风险×0.10
dependency_value = 最高CVE严重度分×0.60 + 归一化CVE数×0.25 + 依赖过时度×0.15
```

**2. 维度风险**（复杂度 / 历史 / 依赖，各 0-100）

```
dimension_risk = 0.5 × 仓库内百分位 + 0.5 × 绝对阈值映射
```

- 仓库内百分位：`percentile = r / 文件数 × 100`，同值取最小 rank（最大值=100、最小值=1/n）；
- 绝对阈值映射采用行业公认经验值（见下表），避免小样本仓库仅靠内部排名人为制造高分文件。

**3. 基础风险分**（0-100）

```
base_risk = 0.6×C + 0.3×H + 0.1×D + 高风险维度惩罚
```

- 高风险维度惩罚：仅当 `max(C,H,D) > 50` 且 `max > mean_of_others × 1.25` 时，追加 `(max - mean_of_others) × 0.15`；
- 单一维度为 100 分位不会封顶总分。

**4. 最终热力值**（0-100）

```
final_heat = base_risk × trend_coefficient
```

- 趋势系数公式为 `1 - trend_delta × 0.3`（历史快照分数变化，理论范围 0.91-1.09）；**当前版本未启用趋势分析（无历史快照），恒为 1.0**。

**仓库总分** = LOC 加权平均 + Top 5% 文件惩罚（`top_files_avg × 0.2`），clamp 0-100。

分数越高，风险越大。一致性系数只用于置信度展示，不参与最终分数。

### 绝对阈值映射标准来源

| 维度 | 指标与分档（低 / 中 / 高 / 严重） | 标准来源 |
| --- | --- | --- |
| 复杂度 | 圈复杂度 `<10 / 10-20 / 20-50 / 50+` | McCabe 圈复杂度阈值 |
| 复杂度 | if 嵌套深度 `≤4 / 5-7 / 8-10 / 11+` | ESLint `max-depth` 规则（默认上限 4） |
| 历史 | 最后修改距今 `0-30 / 31-90 / 91-180 / 180+` 天 | 业界通用的代码陈旧周期 |
| 依赖 | 最高 CVE 严重度 `None / Low / Medium / High / Critical` | CVSS 官方分档 |

各维度绝对分数组合：`复杂度 = 0.7×圈复杂度档 + 0.3×嵌套深度档`；`历史 = 陈旧度档`；`依赖 = 0.6×严重度分 + 0.25×CVE数量档`（过时度暂为 0）。百分位与绝对阈值权重 w1=w2=0.5 为初始值，将用真实项目数据校准。当扫描文件数低于阈值（默认 30，`--min-files` 可配置）时，报告会在显著位置标注"样本量较小，百分位排名统计噪声较大"。

### 漏洞记录去重

同一漏洞可能同时以 GHSA 与 RUSTSEC 两套编号出现（OSV `aliases` 字段互指）。rustburn 通过 aliases 做跨数据源去重，同一漏洞只保留一条记录（优先保留 RUSTSEC 编号，并从合并记录中吸收真实严重度），避免重复计数。

## 防刷分机制

百分位是仓库内**相对排名**：即使攻击者通过拆分函数 / 文件稀释绝对指标，只要全仓库同比例变化，相对排名基本不变（相关测试保证百分位部分相对变化 ≤ 15%）。绝对阈值部分随真实指标诚实变化（拆分会真实降低复杂度绝对分），这是其设计目的。历史重写、空 commit 等行为也会被检测或标记为未知，不会直接降低分数。

## 已知限制

- **临时修改不可检测**：单次扫描无法证明"扫描前临时优化、扫描后恢复"的行为
- **Rename 检测依赖 libgit2 similarity**：未被判定为 rename 的 delete + add 不自行做内容相似度检测
- **趋势历史（v1）**：尚未实现历史 commit 的逐点重算，无有效历史时 `trend_coefficient` 取 1.0（报告标记"趋势数据不足"）
- **离线模式**：依赖漏洞维度标记为数据不完整（Unknown），不会当作 0 处理

## 安全承诺

**绝不静默替换你的可执行文件。** 任何更新动作都有用户可见的确认步骤。

### 更新永远需要显式确认（绝不静默自动更新）

- rustburn **从不**自动更新、**从不**在后台替换二进制；
- `rb update` 先展示当前版本 / 最新版本与 release notes 摘要，必须输入 `y` 确认才继续（`--yes` 可跳过确认）；
- 扫描结束后的版本检测只做提示：24 小时内不重复检查、网络超时 2 秒、失败静默，检测到新版本时仅在终端输出一行"运行 `rb update` 查看并升级"的提示，**不做任何自动操作**；可通过 `--offline` 或 `RUSTBURN_NO_UPDATE_CHECK=1` 完全禁用。

### 原子替换（绝不先删除再写入）

- 新版本先下载到**目标同目录**的临时文件，与官方 `SHA256SUMS` 强制比对通过后，才用 `rename` 完成替换；
- 同目录 `rename` 是原子的：任何时刻目标文件要么是旧版本、要么是新版本，**不存在"先删除旧文件再写入"的窗口**，更新中断也不会留下残缺二进制；
- 校验失败 / 网络失败 / 权限问题都会明确报错并清理临时文件，**绝不覆盖现有可执行文件**（Windows 上目标存在时 `rename` 安全失败，同样不会覆盖）。

## 隐私

rustburn 不上传源码、文件内容、commit message、作者邮箱或仓库路径。
唯一的网络请求包括：
- OSV API 依赖漏洞查询（仅发送包名 / 生态 / 版本）；
- 扫描结束后的版本检测（仅请求 GitHub Releases API 的公开最新版本信息）。

两者均可通过 `--offline` 完全关闭。

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
