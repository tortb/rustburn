use std::fs;
use std::path::Path;
use std::process;
use std::time::Instant;

use chrono::Utc;
use clap::{Parser, Subcommand};

use rustburn_core::complexity::{analyze_complexity, detect_language, FileComplexity};
use rustburn_core::dependency::{
    analyze_dependencies, cargo_to_rust_import, extract_imports_from_source,
};
use rustburn_core::git_history::{analyze_git_history, FileGitMetrics};
use rustburn_core::model::{
    AnalysisMetadata, ConsistencyReport, DependencyFinding, DimensionValues, FileRawMetrics,
    FileScore, Language, RepoReport,
};
use rustburn_core::scoring::{
    calculate_base_risk_score, calculate_consistency_coefficient, calculate_dimension_values,
    calculate_final_heat_score, calculate_percentile_scores, calculate_repo_total_heat_score,
    calculate_top_risk_files, calculate_trend_coefficient,
};
use rustburn_report::write_report;

/// rustburn — 一条命令分析代码仓库中的技术债与潜在风险。
#[derive(Parser)]
#[command(name = "rb", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// 输出文件路径
    #[arg(short, long, global = true)]
    output: Option<String>,

    /// 输出 JSON 报告
    #[arg(long, global = true)]
    json: bool,

    /// 离线模式（禁止网络请求）
    #[arg(long, global = true)]
    offline: bool,

    /// 最大处理的 commit 数量
    #[arg(long, default_value_t = 5000, global = true)]
    max_commits: u32,

    /// 忽略路径模式（可重复，与 .rbignore 合并）
    #[arg(long, global = true)]
    ignore: Vec<String>,

    /// 超过该分数时返回 exit code 1
    #[arg(long, global = true)]
    fail_above: Option<f64>,
}

#[derive(Subcommand)]
enum Commands {
    /// 扫描指定目录（默认当前目录）
    Scan {
        /// 仓库路径
        #[arg(default_value = ".")]
        path: String,
    },
}

/// 文件扫描结果
struct ScannedFile {
    path: String,
    language: Language,
    source: String,
}

/// 扫描统计
struct ScanStats {
    skipped_symlinks: usize,
    skipped_files: usize,
}

/// 单文件大小上限（10 MiB）
const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// .rbignore 默认模板内容
const RBIGNORE_TEMPLATE: &str = "\
# rustburn ignore rules (gitignore-style).
# Edit this file to exclude files and directories from scanning.
#
# Default exclusions:
target/
dist/
out/
";

/// 如果 .rbignore 不存在，自动创建包含默认规则的模板文件。
fn ensure_rbignore(repo_path: &Path) {
    let path = repo_path.join(".rbignore");
    let rbignore_path = path.display().to_string();

    if path.exists() {
        eprintln!(
            "[{}] .rbignore already exists at {}, skipping template creation.",
            Utc::now().format("%Y-%m-%d %H:%M:%S"),
            rbignore_path
        );
        return;
    }

    eprintln!(
        "[{}] .rbignore not found, creating default template at {}",
        Utc::now().format("%Y-%m-%d %H:%M:%S"),
        rbignore_path
    );
    match fs::write(&path, RBIGNORE_TEMPLATE) {
        Ok(()) => eprintln!(
            "[{}] Created .rbignore with default exclusions (target/, dist/, out/).",
            Utc::now().format("%Y-%m-%d %H:%M:%S")
        ),
        Err(e) => eprintln!(
            "[{}] Failed to create .rbignore: {}",
            Utc::now().format("%Y-%m-%d %H:%M:%S"),
            e
        ),
    }
}

/// 读取 .rbignore（gitignore 风格），返回忽略模式列表。
fn read_rbignore(repo_path: &Path) -> Vec<String> {
    let content = match fs::read_to_string(repo_path.join(".rbignore")) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.trim_end_matches('/').to_string())
        .collect()
}

/// 判断相对路径是否被忽略（gitignore 风格：路径前缀 / 目录段 / glob 后缀）。
fn is_ignored(relative: &str, patterns: &[String]) -> bool {
    let segments: Vec<&str> = relative.split('/').collect();
    patterns.iter().any(|p| {
        if relative == p || relative.starts_with(&format!("{}/", p)) {
            return true;
        }
        if segments.contains(&p.as_str()) {
            return true;
        }
        if let Some(suffix) = p.strip_prefix('*') {
            if segments.last().is_some_and(|s| s.ends_with(suffix)) {
                return true;
            }
        }
        false
    })
}

/// 扫描仓库中的文件（不跟随符号链接，遵守 .rbignore / --ignore 合并规则）。
fn scan_files(
    repo_path: &Path,
    ignore_patterns: &[String],
) -> anyhow::Result<(Vec<ScannedFile>, ScanStats)> {
    let mut files = Vec::new();
    let mut stats = ScanStats {
        skipped_symlinks: 0,
        skipped_files: 0,
    };

    fn scan_dir(
        dir: &Path,
        base: &Path,
        files: &mut Vec<ScannedFile>,
        stats: &mut ScanStats,
        ignore: &[String],
    ) -> anyhow::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;

            if file_type.is_symlink() {
                stats.skipped_symlinks += 1;
                continue;
            }
            if file_type.is_dir() {
                scan_dir(&path, base, files, stats, ignore)?;
                continue;
            }
            if !file_type.is_file() {
                continue;
            }

            let relative = path.strip_prefix(base).unwrap_or(&path);
            let relative_str = relative.to_string_lossy().replace('\\', "/");

            if is_ignored(&relative_str, ignore) {
                continue;
            }

            let lang = detect_language(&relative_str);
            if lang == Language::Unknown {
                continue;
            }

            // 超过大小限制的文件跳过
            if entry.metadata()?.len() > MAX_FILE_SIZE {
                stats.skipped_files += 1;
                continue;
            }

            // 二进制 / 无法 UTF-8 解码的文件跳过
            match fs::read_to_string(&path) {
                Ok(source) => files.push(ScannedFile {
                    path: relative_str,
                    language: lang,
                    source,
                }),
                Err(_) => stats.skipped_files += 1,
            }
        }
        Ok(())
    }

    scan_dir(
        repo_path,
        repo_path,
        &mut files,
        &mut stats,
        ignore_patterns,
    )?;
    Ok((files, stats))
}

/// 将依赖发现映射到文件。
fn map_dependencies_to_files(
    files: &[ScannedFile],
    findings: &mut [DependencyFinding],
    _dependencies: &[rustburn_core::dependency::Dependency],
) {
    let mut dep_map: std::collections::HashMap<String, Vec<usize>> =
        std::collections::HashMap::new();

    for (i, finding) in findings.iter().enumerate() {
        let key = if finding.ecosystem == "crates.io" {
            cargo_to_rust_import(&finding.package_name)
        } else {
            finding.package_name.clone()
        };
        dep_map.entry(key).or_default().push(i);
    }

    for file in files {
        let imports = extract_imports_from_source(Path::new(&file.path), &file.source);

        for import in imports {
            if let Some(indices) = dep_map.get(&import) {
                for &idx in indices {
                    if !findings[idx].affected_files.contains(&file.path) {
                        findings[idx].affected_files.push(file.path.clone());
                    }
                }
            }
        }
    }
}

/// 执行扫描，返回 (repo_total_heat_score, fail_above)。
fn run() -> Result<(f64, Option<f64>), String> {
    let cli = Cli::parse();

    let path = match &cli.command {
        Some(Commands::Scan { path }) => path.clone(),
        None => ".".to_string(),
    };

    let start = Instant::now();
    let repo_path = Path::new(&path);

    if !repo_path.exists() {
        return Err(format!("目标目录不存在: {}", path));
    }
    if !repo_path.join(".git").exists() {
        return Err(format!("不是 git 仓库: {}", path));
    }

    // Phase 1: Git 历史分析
    let (git_metrics, history_truncated, history_rewrite) =
        analyze_git_history(repo_path, cli.max_commits).map_err(|e| e.to_string())?;

    // Phase 2: 扫描文件并分析复杂度
    ensure_rbignore(repo_path);
    let ignore_patterns = {
        let mut patterns = read_rbignore(repo_path);
        patterns.extend(cli.ignore.iter().cloned());
        patterns
    };
    let (scanned_files, scan_stats) =
        scan_files(repo_path, &ignore_patterns).map_err(|e| e.to_string())?;

    // Phase 3: 依赖分析
    let dep_analysis = analyze_dependencies(repo_path, cli.offline).map_err(|e| e.to_string())?;
    let mut findings = dep_analysis.findings;
    map_dependencies_to_files(&scanned_files, &mut findings, &dep_analysis.dependencies);

    let mut warnings: Vec<String> = Vec::new();
    if cli.offline {
        warnings.push(
            "Dependency vulnerability scanning is disabled because offline mode is enabled."
                .to_string(),
        );
    }
    if history_truncated {
        warnings.push(format!(
            "History analysis limited to the latest {} commits.",
            cli.max_commits
        ));
    }
    if scanned_files.len() == 1 {
        warnings.push("单文件仓库：percentile 设为 50".to_string());
    }

    // 构建文件指标
    let mut file_metrics_list: Vec<FileRawMetrics> = Vec::new();

    for file in &scanned_files {
        let complexity = match analyze_complexity(&file.source, file.language) {
            Ok(c) => c,
            Err(_) => {
                warnings.push(format!("Warning: failed to parse {}", file.path));
                FileComplexity {
                    loc: 0,
                    cyclomatic_complexity: 1,
                    max_if_nesting_depth: 0,
                    nested_if_ratio: 0.0,
                    avg_function_length: 0.0,
                    max_function_length: 0,
                    parse_incomplete: true,
                }
            }
        };
        if complexity.parse_incomplete {
            warnings.push(format!("Parse warning: {}", file.path));
        }

        let git = git_metrics
            .get(&file.path)
            .cloned()
            .unwrap_or(FileGitMetrics {
                path: file.path.clone(),
                commit_count: 0,
                distinct_authors: 0,
                last_modified_days_ago: 0,
                incident_commit_count: 0,
            });

        let file_findings: Vec<&DependencyFinding> = findings
            .iter()
            .filter(|f| f.affected_files.contains(&file.path))
            .collect();

        let max_severity = file_findings
            .iter()
            .map(|f| f.severity)
            .max()
            .unwrap_or(rustburn_core::model::Severity::None);

        file_metrics_list.push(FileRawMetrics {
            path: file.path.clone(),
            language: file.language,
            loc: complexity.loc,
            cyclomatic_complexity: complexity.cyclomatic_complexity,
            max_if_nesting_depth: complexity.max_if_nesting_depth,
            nested_if_ratio: complexity.nested_if_ratio,
            avg_function_length: complexity.avg_function_length,
            max_function_length: complexity.max_function_length,
            commit_count: git.commit_count,
            distinct_authors: git.distinct_authors,
            last_modified_days_ago: git.last_modified_days_ago,
            incident_commit_count: git.incident_commit_count,
            max_cve_severity: max_severity,
            cve_count: file_findings.len() as u32,
            dependency_staleness: 0.0,
            dependency_data_incomplete: cli.offline || dep_analysis.query_status == "query_failed",
            parse_incomplete: complexity.parse_incomplete,
        });
    }

    // Phase 4: 评分
    let max_commit_count = file_metrics_list
        .iter()
        .map(|m| m.commit_count)
        .max()
        .unwrap_or(1)
        .max(1);
    let max_author_count = file_metrics_list
        .iter()
        .map(|m| m.distinct_authors)
        .max()
        .unwrap_or(1)
        .max(1);
    let max_incident_count = file_metrics_list
        .iter()
        .map(|m| m.incident_commit_count)
        .max()
        .unwrap_or(1)
        .max(1);
    let max_cve_count = file_metrics_list
        .iter()
        .map(|m| m.cve_count)
        .max()
        .unwrap_or(1)
        .max(1);

    let all_dimension_values: Vec<DimensionValues> = file_metrics_list
        .iter()
        .map(|raw| {
            calculate_dimension_values(
                raw,
                max_commit_count,
                max_author_count,
                max_incident_count,
                max_cve_count,
            )
        })
        .collect();

    let mut file_scores: Vec<FileScore> = Vec::new();

    for (i, raw) in file_metrics_list.iter().enumerate() {
        let percentiles =
            calculate_percentile_scores(&all_dimension_values[i], &all_dimension_values);

        let base_risk = calculate_base_risk_score(&percentiles);

        let consistency = ConsistencyReport {
            coverage_report_stale: false,
            history_rewrite,
            lockfile_mismatch: false,
            coefficient: calculate_consistency_coefficient(false, history_rewrite, false),
        };

        let trend_coefficient = calculate_trend_coefficient(&[]);
        let final_heat = calculate_final_heat_score(base_risk, trend_coefficient);

        file_scores.push(FileScore {
            raw: raw.clone(),
            percentiles,
            dimension_values: all_dimension_values[i].clone(),
            base_risk_score: base_risk,
            consistency,
            trend_coefficient,
            final_heat_score: final_heat,
            trend_history: vec![],
        });
    }

    let repo_total = calculate_repo_total_heat_score(&file_scores);
    let top_risk = calculate_top_risk_files(&file_scores);

    let elapsed = start.elapsed().as_secs_f64();

    let report = RepoReport {
        schema_version: "1.0".to_string(),
        rustburn_version: env!("CARGO_PKG_VERSION").to_string(),
        analysis_version: 1,
        repo_path: path.clone(),
        scanned_at: Utc::now().to_rfc3339(),
        files: file_scores,
        repo_total_heat_score: repo_total,
        top_risk_files: top_risk,
        dependency_findings: findings,
        anomalies: vec![],
        analysis_metadata: AnalysisMetadata {
            max_commits: cli.max_commits,
            history_truncated,
            offline: cli.offline,
            osv_status: dep_analysis.query_status,
            supported_languages: vec!["rust".to_string(), "javascript".to_string()],
            elapsed_seconds: elapsed,
            file_count: scanned_files.len(),
            skipped_symlinks: scan_stats.skipped_symlinks,
            skipped_files: scan_stats.skipped_files,
        },
        warnings,
    };

    // 输出
    let output = cli.output.unwrap_or_else(|| {
        if cli.json {
            "rustburn-report.json".to_string()
        } else {
            "rustburn-report.html".to_string()
        }
    });

    if cli.json {
        let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
        fs::write(&output, json).map_err(|e| format!("无法写入报告: {}", e))?;
    } else {
        write_report(&report, Path::new(&output)).map_err(|e| format!("无法写入报告: {}", e))?;
    }

    // 简洁终端摘要
    let file_count = scanned_files.len();
    let total_loc: u32 = report.files.iter().map(|f| f.raw.loc).sum();
    let n = file_count.max(1) as f64;
    let avg_complexity = report
        .files
        .iter()
        .map(|f| f.dimension_values.complexity_value)
        .sum::<f64>()
        / n;
    let avg_history = report
        .files
        .iter()
        .map(|f| f.dimension_values.history_value)
        .sum::<f64>()
        / n;
    let avg_dependency = report
        .files
        .iter()
        .map(|f| f.dimension_values.dependency_value)
        .sum::<f64>()
        / n;

    eprintln!("rustburn v{}", env!("CARGO_PKG_VERSION"));
    eprintln!("Scanning {}", path);
    eprintln!("Files       {}", file_count);
    eprintln!("LOC         {}", total_loc);
    eprintln!("Complexity  {:.1}", avg_complexity);
    eprintln!("History     {:.1}", avg_history);
    eprintln!("Dependency  {:.1}", avg_dependency);
    eprintln!("Repository heat score: {:.1} / 100", repo_total);
    eprintln!("Report:");
    eprintln!("  {}", output);

    Ok((repo_total, cli.fail_above))
}

fn main() {
    let code = match run() {
        Ok((score, threshold)) => {
            if let Some(t) = threshold {
                if score > t {
                    eprintln!(
                        "Repository heat score {:.2} exceeds threshold {:.2}",
                        score, t
                    );
                    1
                } else {
                    0
                }
            } else {
                0
            }
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            2
        }
    };
    process::exit(code);
}
