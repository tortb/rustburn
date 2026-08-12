use std::fs;
use std::io::Write;
use std::path::Path;
use std::process;
use std::time::Instant;

use chrono::Utc;
use clap::{Parser, Subcommand};

use rustburn_core::complexity::{analyze_complexity, detect_language};
use rustburn_core::dependency::{
    analyze_dependencies, cargo_to_rust_import, extract_imports_from_source,
};
use rustburn_core::git_history::{analyze_git_history, FileGitMetrics};
use rustburn_core::model::{
    AnalysisMetadata, ConsistencyReport, DependencyFinding, DimensionValues, FileRawMetrics,
    FileScore, Language, OutputFormat, RepoReport,
};
use rustburn_core::scoring::{
    calculate_base_risk_score, calculate_consistency_coefficient, calculate_dimension_values,
    calculate_final_heat_score, calculate_percentile_scores, calculate_repo_total_heat_score,
    calculate_trend_coefficient, get_top_risk_files,
};
use rustburn_report::write_report;

/// rustburn — 一条命令分析代码仓库中的技术债与潜在风险。
#[derive(Parser)]
#[command(name = "rustburn", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 扫描代码仓库
    Scan {
        /// 仓库路径
        #[arg(default_value = ".")]
        path: String,

        /// 输出文件路径
        #[arg(long, default_value = "rustburn-report.html")]
        output: String,

        /// 最大处理的 commit 数量
        #[arg(long, default_value_t = 5000)]
        max_commits: u32,

        /// 离线模式（禁止网络请求）
        #[arg(long, default_value_t = false)]
        offline: bool,

        /// 忽略路径模式
        #[arg(long = "ignore")]
        ignore: Vec<String>,

        /// 输出格式 (html|json)
        #[arg(long, default_value = "html")]
        format: String,

        /// 超过该分数时返回 exit code 1
        #[arg(long)]
        fail_above: Option<f64>,
    },
}

/// 文件扫描结果
struct ScannedFile {
    path: String,
    language: Language,
    source: String,
}

/// 扫描仓库中的文件
fn scan_files(repo_path: &Path, ignore_patterns: &[String]) -> anyhow::Result<Vec<ScannedFile>> {
    let mut files = Vec::new();

    fn scan_dir(
        dir: &Path,
        base: &Path,
        files: &mut Vec<ScannedFile>,
        ignore: &[String],
    ) -> anyhow::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                let dir_name = path.file_name().unwrap_or_default().to_string_lossy();
                // 跳过隐藏目录和常见非源码目录
                if dir_name.starts_with('.') || dir_name == "node_modules" || dir_name == "target" {
                    continue;
                }
                scan_dir(&path, base, files, ignore)?;
            } else if path.is_file() {
                let relative = path.strip_prefix(base).unwrap_or(&path);
                let relative_str = relative.to_string_lossy().replace('\\', "/");

                // 检查忽略模式
                if ignore
                    .iter()
                    .any(|pattern| relative_str.contains(pattern.as_str()))
                {
                    continue;
                }

                let lang = detect_language(&relative_str);
                if lang == Language::Unknown {
                    continue;
                }

                match fs::read_to_string(&path) {
                    Ok(source) => {
                        files.push(ScannedFile {
                            path: relative_str,
                            language: lang,
                            source,
                        });
                    }
                    Err(_) => continue,
                }
            }
        }
        Ok(())
    }

    scan_dir(repo_path, repo_path, &mut files, ignore_patterns)?;
    Ok(files)
}

/// 将依赖发现映射到文件
fn map_dependencies_to_files(
    files: &[ScannedFile],
    findings: &mut [DependencyFinding],
    _dependencies: &[rustburn_core::dependency::Dependency],
) {
    // 构建依赖名称到 finding 的映射
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

    // 遍历每个文件，提取 imports 并匹配
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

/// 解析输出格式
fn parse_format(format: &str) -> Result<OutputFormat, String> {
    match format {
        "html" => Ok(OutputFormat::Html),
        "json" => Ok(OutputFormat::Json),
        _ => Err(format!("不支持的输出格式: {}。支持: html, json", format)),
    }
}

fn run_scan() -> Result<(f64, OutputFormat, String), String> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Scan {
            path,
            output,
            max_commits,
            offline,
            ignore,
            format,
            fail_above,
        } => {
            let output_format = parse_format(&format)?;
            let start = Instant::now();
            let repo_path = Path::new(&path);

            // 验证仓库路径
            if !repo_path.exists() {
                return Err(format!("仓库路径不存在: {}", path));
            }

            // 检查是否为 git 仓库
            if !repo_path.join(".git").exists() {
                return Err(format!("不是 git 仓库: {}", path));
            }

            eprintln!("🔥 Rustburn v{}", env!("CARGO_PKG_VERSION"));
            eprintln!("正在分析仓库: {}", path);

            // Phase 1: Git 历史分析
            eprintln!("📊 分析 Git 历史...");
            let (git_metrics, history_truncated, history_rewrite) =
                analyze_git_history(repo_path, max_commits).map_err(|e| e.to_string())?;

            // Phase 2: 扫描文件并分析复杂度
            eprintln!("📁 扫描文件...");
            let scanned_files = scan_files(repo_path, &ignore).map_err(|e| e.to_string())?;
            eprintln!("  找到 {} 个源码文件", scanned_files.len());

            // Phase 3: 依赖分析
            eprintln!("🔒 分析依赖...");
            let dep_analysis =
                analyze_dependencies(repo_path, offline).map_err(|e| e.to_string())?;
            let mut findings = dep_analysis.findings;
            map_dependencies_to_files(&scanned_files, &mut findings, &dep_analysis.dependencies);

            // 构建文件指标
            let mut file_metrics_list: Vec<FileRawMetrics> = Vec::new();

            for file in &scanned_files {
                let complexity = analyze_complexity(&file.source, file.language).unwrap_or(
                    rustburn_core::complexity::FileComplexity {
                        loc: 0,
                        cyclomatic_complexity: 1,
                        max_if_nesting_depth: 0,
                        nested_if_ratio: 0.0,
                        avg_function_length: 0.0,
                        max_function_length: 0,
                        parse_incomplete: true,
                    },
                );

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

                // 计算文件的最大 CVE 严重度
                let file_findings: Vec<&DependencyFinding> = findings
                    .iter()
                    .filter(|f| f.affected_files.contains(&file.path))
                    .collect();

                let max_severity = file_findings
                    .iter()
                    .map(|f| f.severity)
                    .max()
                    .unwrap_or(rustburn_core::model::Severity::None);

                let raw = FileRawMetrics {
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
                    dependency_data_incomplete: dep_analysis.query_status == "query_failed",
                    parse_incomplete: complexity.parse_incomplete,
                };

                file_metrics_list.push(raw);
            }

            // Phase 4: 评分
            eprintln!("📈 计算评分...");

            // 计算全局最大值用于归一化
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

            // 计算所有文件的维度综合值
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

            // 单文件仓库 warning
            let mut warnings: Vec<String> = Vec::new();
            if file_metrics_list.len() == 1 {
                warnings.push("单文件仓库：percentile 设为 50".to_string());
            }

            let mut file_scores: Vec<FileScore> = Vec::new();

            for (i, raw) in file_metrics_list.iter().enumerate() {
                let percentiles = calculate_percentile_scores(raw, &all_dimension_values);

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

            // 构建报告
            let repo_total = calculate_repo_total_heat_score(&file_scores);
            let top_risk = get_top_risk_files(&file_scores);

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
                    max_commits,
                    history_truncated,
                    offline,
                    osv_status: dep_analysis.query_status,
                    supported_languages: vec!["rust".to_string(), "javascript".to_string()],
                    elapsed_seconds: elapsed,
                    file_count: scanned_files.len(),
                    skipped_symlinks: 0,
                    skipped_files: 0,
                },
                warnings,
            };

            // 输出
            match output_format {
                OutputFormat::Html => {
                    eprintln!("📝 生成报告...");
                    let output_path = Path::new(&output);
                    write_report(&report, output_path).map_err(|e| e.to_string())?;

                    eprintln!();
                    eprintln!("✅ 分析完成！");
                    eprintln!("  仓库总热度分数: {:.2}", repo_total);
                    eprintln!("  分析文件数: {}", scanned_files.len());
                    eprintln!("  耗时: {:.2} 秒", elapsed);
                    eprintln!("  报告已保存到: {}", output);
                }
                OutputFormat::Json => {
                    let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
                    // JSON 输出到 stdout
                    let stdout = std::io::stdout();
                    let mut handle = stdout.lock();
                    handle
                        .write_all(json.as_bytes())
                        .map_err(|e| e.to_string())?;
                    handle.write_all(b"\n").map_err(|e| e.to_string())?;
                }
            }

            // 检查 fail_above
            if let Some(threshold) = fail_above {
                if repo_total > threshold {
                    return Err(format!(
                        "仓库热度分数 {:.2} 超过阈值 {}",
                        repo_total, threshold
                    ));
                }
            }

            Ok((repo_total, output_format, output))
        }
    }
}

fn main() {
    match run_scan() {
        Ok(_) => process::exit(0),
        Err(e) => {
            // 判断是否为阈值超过
            if e.contains("超过阈值") {
                eprintln!("❌ {}", e);
                process::exit(1);
            } else if e.contains("不存在") || e.contains("不是 git 仓库") {
                eprintln!("❌ {}", e);
                process::exit(3);
            } else if e.contains("不支持的输出格式") {
                eprintln!("❌ {}", e);
                process::exit(2);
            } else {
                eprintln!("❌ {}", e);
                process::exit(4);
            }
        }
    }
}
