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
- Go（`.go`）——含 `go.sum` 依赖扫描（OSV `Go` 生态）、`go tool cover` 覆盖率报告（自动转换为 Cobertura）、`{name}_test.go` 测试文件识别

其他语言自动跳过，不会导致扫描失败。符号链接默认不跟随。

语言支持通过 `LanguageAdapter` 适配器扩展：每新增一种语言只需实现该 trait 并在注册表登记一行，五个评分维度（复杂度 / 重复 / 测试 / 变更风险 / 依赖）零改动即可复用（有架构解耦测试保障）。测试约定与锁文件格式等语言特定知识通过 `LanguageProfile` 配置表按语言注册。

## HTML 报告内容

生成的 `rustburn-report.html` 为自包含单文件，包含：

- **总览**：仓库总热力值（conic 环形进度条）、文件数、LOC、Top 5% 占比、平均置信度
- **文件树热力图**：面积 = LOC，颜色 = 热力分数；超过 500 个文件时自动降级为 Top 100 + 折叠其余
- **Top 风险文件**：可展开卡片，同一屏幕内并排展示五个维度的独立风险分与置信度，综合参考分作为辅助排序
- **文件详情**：原始指标、五维度明细（风险分 / 原始值 / 置信度）、趋势曲线（Canvas 手写，含 commit sha / 日期 / 分数 tooltip）、一致性检查、公式推导
- **依赖漏洞**：按严重度排序的 CVE / GHSA / OSV 发现
- **异常标记**：历史重写、临时复杂度下降等疑点

## 评分模型（v2：五维度）

rustburn 的评分完全透明，每一层公式与源码实现严格一致。v2 架构把评分拆成两层：

**1. 五个维度独立分析器**（各输出 0-100 独立风险分）

| 维度 | 输入 | 风险分来源 |
| --- | --- | --- |
| complexity（复杂度） | 语言 AST | 样本 ≥5 时 0.5 × 仓库内百分位 + 0.5 × 绝对阈值（McCabe / ESLint max-depth）；样本 <5 时仅绝对阈值 |
| duplication（重复代码） | 语言 AST | min(100, 结构哈希重复行占比 × 150) |
| test（测试质量） | 文件命名约定 + lcov/cobertura 覆盖率 | 覆盖率缺口 ×0.4 + 测试密度缺口 ×0.25 + 断言密度缺口 ×0.35，零断言触发 +15 空壳测试惩罚 |
| change_risk（变更风险） | git 历史时间线 | 近期事故密度 ×0.6 + 近期改动频率 ×0.2 + 作者分散度 ×0.2 |
| dependency（依赖风险） | 锁文件 + OSV 漏洞查询 | CVSS 严重度 ×0.6 + CVE 数量档位 ×0.25 |

**2. 归一层（scoring.rs）**

```
base_risk = 复杂度 30% + 重复代码 15% + 测试 25% + 变更风险 20% + 依赖 10%
          + 单一高风险维度惩罚
final_heat = base_risk × trend_coefficient   # 本版未启用趋势，恒为 1.0
```

- 高风险维度惩罚：仅当某维度显著偏离其余维度均值（max > 50 且 max > mean_of_others × 1.25）时，追加 `(max - mean_of_others) × 0.15`；
- 某维度 `NotApplicable`（如语言暂不支持重复检测）时，该维度权重按比例分摊到其余维度重新归一，并在报告中标注；
- 某维度数据缺失（`DataMissing`）时，风险分用仓库其他文件的均值填充并标记置信度，**不会**当作 0 处理。

**仓库总分** = LOC 加权平均 + Top 5% 文件惩罚（`top_files_avg × 0.2`），clamp 0-100。

分数越高，风险越大。置信度只用于展示，不参与最终分数。

### 变更风险：时间衰减（不再"只涨不跌"）

`change_risk` 只消费每个文件的 commit 时间戳，事故密度 = 衰减加权的 incident / 全部 commit 比值，衰减权重 `0.5 ^ (距今月数 / 6)`（半衰期 6 个月），时间基准为分析时的当前时间。**任何环节不使用终身累计 commit 数**——一个文件即使历史事故很多，只要近期保持稳定，风险会随时间自然回落。

### 测试质量维度

对应测试文件按以下优先级匹配（命中即停止）：

1. 同目录 `<文件名>_test.<ext>` / `<文件名>.test.<ext>` / `test_<文件名>.<ext>`；
2. Rust 文件内部 `#[cfg(test)] mod tests` 块（分母扣除 test mod 行数）；
3. `tests/` 目录 + 可配置正则映射（默认尝试 `src/`、`lib/`、`app/` 源根）；
4. 找不到 → 测试维度标记 `NotApplicable`，权重按比例分摊到其余维度（不参与本次合成的维度在报告中显著标注）。

### 重复代码：结构哈希

对每个超过 6 行的函数生成"结构哈希"：所有标识符替换为占位符，保留语法结构与字面量**类型**（数字/字符串/布尔，不保留值）。哈希相同的块跨文件归组，组内 ≥2 个成员即计入重复。文本级逐行判重和丢弃字面量类型都会导致误判，已被禁止。

### 漏洞记录去重

同一漏洞可能同时以 GHSA 与 RUSTSEC 两套编号出现（OSV `aliases` 字段互指）。rustburn 通过 aliases 做跨数据源去重，同一漏洞只保留一条记录（优先保留 RUSTSEC 编号，并从合并记录中吸收真实严重度），避免重复计数。任何 CVE 记录必须直接来自 OSV API 的真实响应，测试桩数据不会出现在正常运行路径。

## 防刷分机制

百分位是仓库内**相对排名**：即使攻击者通过拆分函数 / 文件稀释绝对指标，只要全仓库同比例变化，相对排名基本不变（相关测试保证百分位部分相对变化 ≤ 15%）。绝对阈值部分随真实指标诚实变化（拆分会真实降低复杂度绝对分），这是其设计目的。历史重写、空 commit 等行为也会被检测或标记为未知，不会直接降低分数。

## 已知限制

- **临时修改不可检测**：单次扫描无法证明"扫描前临时优化、扫描后恢复"的行为
- **Rename 检测依赖 libgit2 similarity**：未被判定为 rename 的 delete + add 不自行做内容相似度检测
- **趋势历史（v1）**：尚未实现历史 commit 的逐点重算，无有效历史时 `trend_coefficient` 取 1.0（报告标记"趋势数据不足"）
- **离线模式**：依赖漏洞维度标记为数据缺失（`DataMissing`），风险分按中性值填充，不会当作 0 处理；报告会显著标注
- **覆盖率报告**：仅识别仓库常见路径下的 `lcov.info` / `cobertura.xml`；未提供覆盖率报告时，测试维度的覆盖率缺口按仓库均值填充并标记数据缺失
- **测试文件匹配**：依赖命名约定（`*_test.*` / `*.test.*` / `test_*.*` / Rust 内部 `#[cfg(test)] mod`），不符合约定的测试组织方式可能识别不到

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

- `crates/rustburn-cli` — CLI 入口（二进制 `rb`），负责编排分析流水线
- `crates/rustburn-core` — 算法核心：`lang/`（LanguageAdapter 语言适配层）、`analyzers/`（五个维度分析器）、`context.rs`（分析上下文）、`scoring.rs`（归一合成）、`git_history.rs` / `dependency.rs` / `aggregate.rs`
- `crates/rustburn-report` — HTML 报告渲染（askama 模板）

## 参考

- [spec.md](spec.md) — 项目开发规格文档
- [cli-spec.md](cli-spec.md) — CLI 功能规格
