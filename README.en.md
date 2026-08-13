# rustburn

English | [中文](README.md)

A single-command code technical debt analyzer that produces a self-contained HTML report.

- **Zero external dependencies**: the report relies on no CDN, external JS, or external fonts — it opens offline and is safe to share as a screenshot
- **Transparent scoring**: every intermediate value of the three-layer formula (dimension composites / percentiles / base risk score / trend coefficient) is traceable and verifiable
- **Privacy-friendly**: no source code is uploaded; the only network request is the OSV dependency lookup, which can be fully disabled

## Installation

### One-line install (GitHub Releases)

```sh
curl -fsSL https://raw.githubusercontent.com/tortb/rustburn/master/install.sh | sh
```

The installer resolves the latest release dynamically, verifies the SHA256 checksum, and installs to `~/.local/bin/rb` (no sudo required). It aborts if download, checksum, or extraction fails, and never breaks an existing installation. If `~/.local/bin` is not on your PATH it prints a hint and never edits your shell configuration.

### Build from source

```sh
git clone https://github.com/tortb/rustburn.git
cd rustburn
cargo build --release
# The binary is at target/release/rb; copy it anywhere on your PATH
install -m 755 target/release/rb ~/.local/bin/rb
```

## Quick start

```sh
rb                 # Scan the current directory, write rustburn-report.html
rb scan ./project  # Scan a specific directory
rb --json          # Write a JSON report (default rustburn-report.json)
rb --offline       # Offline mode (no network requests, update check disabled too)
rb --fail-above 70 # Exit with code 1 when the score exceeds 70 (CI-friendly)
rb --ignore target # Exclude a path (repeatable)
rb update          # Check and update to the latest GitHub release (interactive confirmation)
rb --version       # Version number + git commit hash + build date
```

### Options

| Option | Description | Default |
| --- | --- | --- |
| `-o, --output <FILE>` | Output file path | `rustburn-report.html` / `rustburn-report.json` |
| `--json` | Write a JSON report | no |
| `--offline` | Disable network requests | no |
| `--max-commits <N>` | Maximum number of commits to analyze | `5000` |
| `--ignore <PATTERN>` | Exclude a path (repeatable, merged with `.rbignore`) | none |
| `--fail-above <SCORE>` | Exit with code 1 when the score exceeds the threshold | none |
| `-h, --help` / `-V, --version` | Help / version | - |

### Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Scan succeeded |
| `1` | Score exceeded the `--fail-above` threshold |
| `2` | Execution error (missing path, not a git repo, write failure, etc.) |

Ordinary warnings (AST parse failures, dependency query failures, truncated history) do not cause a non-zero exit.

## Exclusions

A `.rbignore` file at the repository root (gitignore-style) is merged with `--ignore`:

```text
node_modules/
dist/
target/
*.min.js
*.generated.rs
```

rustburn ignores **nothing** by default (including `.git/`, `target/`, `node_modules/`). Add patterns to `.rbignore` or use `--ignore` to exclude directories.

## Supported languages

- Rust (`.rs`)
- JavaScript / JSX (`.js`, `.jsx`)

Other languages are skipped without failing the scan. Symbolic links are not followed.

## HTML report contents

The generated `rustburn-report.html` is a self-contained single file that includes:

- **Overview**: repository heat score (conic-gradient ring), file count, LOC, Top 5% share, average confidence
- **File tree heatmap**: area = LOC, color = heat score; automatically degrades to Top 100 + collapsed remainder beyond 500 files
- **Top risk files**: expandable cards showing three-layer formula intermediates and percentile bars
- **File details**: raw metrics, percentiles, trend curve (hand-written Canvas with commit sha / date / score tooltips), consistency checks, formula derivation
- **Dependency findings**: CVE / GHSA / OSV findings sorted by severity
- **Anomalies**: history rewrite, temporary complexity drops, and other red flags

## Scoring model (four-layer formula)

rustburn's scoring is fully transparent; every layer below matches the source code exactly.

**1. Dimension composite values** (0-100, from raw metrics)

```
complexity_value = cyclomatic_complexity×0.4 + max_if_nesting_depth×15×0.4 + avg_function_length×0.2
history_value    = normalized_commits×0.35 + normalized_authors×0.10 + normalized_incidents×0.45 + staleness_risk×0.10
dependency_value = max_cve_severity_score×0.60 + normalized_cve_count×0.25 + dependency_staleness×0.15
```

**2. Dimension risk** (complexity / history / dependency, each 0-100)

```
dimension_risk = 0.5 × repository percentile + 0.5 × absolute threshold mapping
```

- Repository percentile: `percentile = r / file_count × 100`, ties use the minimum rank (max=100, min=1/n);
- The absolute mapping uses widely accepted industry thresholds (see table below) so small repositories cannot manufacture high-risk files purely from internal ranking.

**3. Base risk score** (0-100)

```
base_risk = 0.6×C + 0.3×H + 0.1×D + high-risk dimension penalty
```

- High-risk dimension penalty: only when `max(C,H,D) > 50` and `max > mean_of_others × 1.25`, add `(max - mean_of_others) × 0.15`;
- A single dimension at the 100th percentile never caps the total.

**4. Final heat score** (0-100)

```
final_heat = base_risk × trend_coefficient
```

- The trend coefficient formula is `1 - trend_delta × 0.3` (change in historical snapshot scores, theoretical range 0.91-1.09); **the current version has trend analysis disabled (no historical snapshots), so it is always 1.0**.

**Repository score** = LOC-weighted mean + Top 5% penalty (`top_files_avg × 0.2`), clamped to 0-100.

Higher scores mean higher risk. The consistency coefficient is used only for confidence display and never participates in the final score.

### Absolute threshold mapping — standard sources

| Dimension | Metric and bands (low / medium / high / severe) | Source |
| --- | --- | --- |
| Complexity | Cyclomatic complexity `<10 / 10-20 / 20-50 / 50+` | McCabe cyclomatic complexity thresholds |
| Complexity | If-nesting depth `≤4 / 5-7 / 8-10 / 11+` | ESLint `max-depth` rule (default max 4) |
| History | Days since last change `0-30 / 31-90 / 91-180 / 180+` | Common code-staleness periods |
| Dependency | Highest CVE severity `None / Low / Medium / High / Critical` | Official CVSS bands |

Absolute score composition per dimension: `complexity = 0.7×cc_band + 0.3×depth_band`; `history = staleness_band`; `dependency = 0.6×severity_score + 0.25×cve_count_band` (staleness currently 0). The initial weights w1=w2=0.5 will be calibrated against real projects. When the scanned file count is below the threshold (default 30, configurable via `--min-files`), the report prominently notes "small sample size: percentile ranking is noisy".

### Vulnerability deduplication

The same vulnerability can appear under both GHSA and RUSTSEC identifiers (their OSV `aliases` fields point at each other). rustburn deduplicates across data sources using aliases, keeping one record per vulnerability (preferring the RUSTSEC id while absorbing the real severity from merged records), avoiding double counting.

## Anti-gaming

Percentiles are **relative ranks** within the repository: even if an attacker dilutes absolute metrics by splitting functions or files, a proportional change across the whole repository leaves the relative ranks essentially unchanged (tests guarantee a relative change of ≤ 15% for the percentile part). The absolute-threshold part honestly follows real metrics (splitting genuinely lowers the absolute complexity score), which is its purpose. History rewrites and empty commits are detected or marked as unknown rather than directly lowering scores.

## Known limitations

- **Temporary modifications cannot be detected**: a single scan cannot prove "optimized before the scan, restored after"
- **Rename detection relies on libgit2 similarity**: delete + add pairs not classified as renames are not similarity-checked
- **Trend history (v1)**: per-commit recomputation is not implemented yet; `trend_coefficient` is 1.0 without valid history (marked "insufficient trend data" in the report)
- **Offline mode**: the dependency dimension is marked as incomplete (Unknown) rather than treated as 0

## Security commitment

**We never silently replace your executable.** Every update action requires a visible confirmation step:

- `rb update` first shows the current / latest version and a release notes summary, and only proceeds after you type `y` (`--yes` skips the prompt);
- The new binary is downloaded to a temporary file and hard-verified against the official `SHA256SUMS` before being installed with an atomic `rename`;
- On checksum or network failure the tool reports a clear error and cleans up temporary files, **never overwriting the existing executable**;
- During the atomic replace the target is always either the old or the new binary — never a partial file.

The post-scan version check is equally conservative: it runs at most once per 24 hours, uses a 2-second network timeout, fails silently, and only prints a one-line hint when a newer version is found — it never takes any automatic action. Disable it entirely with `--offline` or `RUSTBURN_NO_UPDATE_CHECK=1`.

## Privacy

rustburn never uploads source code, file contents, commit messages, author emails, or repository paths.
The only network requests are:
- the OSV dependency lookup (sending only package name / ecosystem / version);
- the post-scan version check (fetching public latest-release info from the GitHub Releases API).

Both are fully disableable via `--offline`.

## Development

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --check
```

Workspace layout:

- `crates/rustburn-cli` — CLI entry point (binary `rb`)
- `crates/rustburn-core` — complexity / git history / dependency / scoring algorithms
- `crates/rustburn-report` — HTML report rendering (askama template)

## References

- [spec.md](spec.md) — project development specification (Chinese)
- [cli-spec.md](cli-spec.md) — CLI feature specification (Chinese)
