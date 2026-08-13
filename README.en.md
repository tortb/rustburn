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

## Scoring model (three-layer formula)

1. **Base risk score** = `100 - cbrt((100 - C) × (100 - H) × (100 - D))`, where C / H / D are the repository-wide percentile ranks (0-100, higher is worse) of the complexity, history, and dependency dimensions
2. **Trend coefficient** = `1 - trend_delta × 0.3` (change in historical snapshot scores, range 0.91-1.09)
3. **Final heat score** = base risk score × trend coefficient (clamped to 0-100)

The repository score = LOC-weighted mean + Top 5% penalty (`top_files_avg × 0.2`).

Higher scores mean higher risk. The consistency coefficient is used only for confidence display and never participates in the final score.

## Anti-gaming

Percentiles are **relative ranks** within the repository: even if an attacker dilutes absolute metrics by splitting functions or files, a proportional change across the whole repository leaves relative ranks and base risk scores essentially unchanged (tests guarantee a relative change of ≤ 15%). History rewrites and empty commits are detected or marked as unknown rather than directly lowering scores.

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
