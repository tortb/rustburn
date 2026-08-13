use std::fs;
use std::path::Path;
use std::process;
use std::time::{Duration, Instant};

use chrono::Utc;
use clap::{Parser, Subcommand};
use tree_sitter::Tree;

use rustburn_core::aggregate::{calculate_repo_total_heat_score, calculate_top_risk_files};
use rustburn_core::analyzer::DimensionAnalyzer;
use rustburn_core::analyzers::change_risk::{change_risk_value, ChangeRiskAnalyzer};
use rustburn_core::analyzers::complexity::{
    absolute_complexity_score, complexity_raw_value, repo_percentile, ComplexityAnalyzer,
};
use rustburn_core::analyzers::dependency::{dependency_risk, DependencyAnalyzer};
use rustburn_core::analyzers::duplication::{
    build_duplication_groups, duplication_risk_from_ranges, DuplicationAnalyzer,
    DuplicationFileInput,
};
use rustburn_core::analyzers::test::{
    build_test_context, TestAnalyzer, TestFileInput, TestPathRules,
};
use rustburn_core::complexity::{detect_language, FileComplexity};
use rustburn_core::context::{DependencyFileData, FileContext, GitTimeline, RepoAnalysisData};
use rustburn_core::dependency::{
    analyze_dependencies, cargo_to_rust_import, extract_imports_from_source,
};
use rustburn_core::git_history::{analyze_git_history, analyze_git_timelines, FileGitMetrics};
use rustburn_core::lang::{adapter_for, LanguageAdapter};
use rustburn_core::model::{
    AnalysisMetadata, ConsistencyReport, DependencyFinding, DimensionResult, FileRawMetrics,
    FileScore, Language, RepoReport, Severity,
};
use rustburn_core::scoring::{
    calculate_base_risk_score, calculate_consistency_coefficient, calculate_final_heat_score,
    calculate_trend_coefficient,
};
use rustburn_core::update::{
    cache_dir, check_update_silently, is_newer, latest_release, notes_summary, platform_target,
    update_check_enabled, update_to_latest, DEFAULT_API_URL, DEFAULT_DL_BASE,
};
use rustburn_report::write_report;

/// 版本信息：版本号 + git commit 短哈希 + 构建日期（由 build.rs 注入）。
const RB_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (commit: ",
    env!("RUSTBURN_GIT_COMMIT"),
    ", built: ",
    env!("RUSTBURN_BUILD_DATE"),
    ")"
);

/// rustburn — 一条命令分析代码仓库中的技术债与潜在风险。
#[derive(Parser)]
#[command(name = "rb", version = RB_VERSION, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// 输出文件路径
    #[arg(short, long, global = true)]
    output: Option<String>,

    /// 输出 JSON 报告
    #[arg(long, global = true)]
    json: bool,

    /// 离线模式（禁止网络请求，同时禁用更新检测）
    #[arg(long, global = true)]
    offline: bool,

    /// 最大处理的 commit 数量
    #[arg(long, default_value_t = 5000, global = true)]
    max_commits: u32,

    /// 忽略路径模式（可重复，与 .rbignore 合并）
    #[arg(long, global = true)]
    ignore: Vec<String>,

    /// 包含生成的产物路径（target/、node_modules/、dist/、build/、*.generated.*），默认排除
    #[arg(long, global = true)]
    include_generated: bool,

    /// 超过该分数时返回 exit code 1
    #[arg(long, global = true)]
    fail_above: Option<f64>,

    /// 样本量阈值：文件数低于该值时在报告中标注"百分位统计噪声较大"
    #[arg(long, default_value_t = 30, global = true)]
    min_files: u32,
}

#[derive(Subcommand)]
enum Commands {
    /// 扫描指定目录（默认当前目录）
    Scan {
        /// 仓库路径
        #[arg(default_value = ".")]
        path: String,
    },
    /// 检查并更新到 GitHub 最新发布版本
    Update {
        /// 跳过交互确认，直接更新
        #[arg(long)]
        yes: bool,
        /// （测试/调试用）自定义 GitHub API URL
        #[arg(long, hide = true)]
        api_url: Option<String>,
        /// （测试/调试用）自定义 release 下载基址
        #[arg(long, hide = true)]
        dl_base: Option<String>,
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

/// 单个文件解析后的分析输入。
struct ParsedFile {
    scanned: ScannedFile,
    tree: Option<Tree>,
    loc: u32,
    parse_incomplete: bool,
    complexity: FileComplexity,
    git: GitTimeline,
    dep: DependencyFileData,
}

/// 单文件大小上限（10 MiB）
const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// .rbignore 默认模板内容
const RBIGNORE_TEMPLATE: &str = "\
# rustburn ignore rules (gitignore-style).
# Edit this file to exclude files and directories from scanning.
#
# 注意：生成产物目录（target/、node_modules/、dist/、build/、*.generated.*）
# 由内置默认规则排除，无需在此重复；如需包含它们，请使用 --include-generated。
";

/// 强制默认排除的路径模式（gitignore 风格）。
const FORCED_IGNORE_PATTERNS: &[&str] =
    &["target", "node_modules", "dist", "build", "*.generated.*"];

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
            "[{}] Created .rbignore with default exclusions.",
            Utc::now().format("%Y-%m-%d %H:%M:%S")
        ),
        Err(e) => eprintln!(
            "[{}] Failed to create .rbignore: {}",
            Utc::now().format("%Y-%m-%d %H:%M:%S"),
            e
        ),
    }
}

/// 扫描仓库中的文件。
fn scan_files(
    repo_path: &Path,
    cli_ignore_patterns: &[String],
    include_generated: bool,
) -> anyhow::Result<(Vec<ScannedFile>, ScanStats)> {
    let mut override_builder = ignore::overrides::OverrideBuilder::new(repo_path);
    if !include_generated {
        for pattern in FORCED_IGNORE_PATTERNS {
            override_builder.add(&format!("!{}", pattern))?;
        }
    }
    for pattern in cli_ignore_patterns {
        override_builder.add(&format!("!{}", pattern))?;
    }
    let overrides = override_builder.build()?;

    let mut walker = ignore::WalkBuilder::new(repo_path);
    walker
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .ignore(true)
        .parents(true)
        .follow_links(false)
        .overrides(overrides)
        .add_custom_ignore_filename(".rbignore");

    let mut files = Vec::new();
    let mut stats = ScanStats {
        skipped_symlinks: 0,
        skipped_files: 0,
    };

    for result in walker.build() {
        let entry = result?;
        let path = entry.path();

        match entry.file_type() {
            Some(ft) if ft.is_symlink() => {
                stats.skipped_symlinks += 1;
                continue;
            }
            Some(ft) if !ft.is_file() => continue,
            _ => {}
        }

        let relative = path.strip_prefix(repo_path).unwrap_or(path);
        let relative_str = relative.to_string_lossy().replace('\\', "/");

        let lang = detect_language(&relative_str);
        if lang == Language::Unknown {
            continue;
        }

        let file_len = entry.metadata()?.len();
        if file_len > MAX_FILE_SIZE {
            stats.skipped_files += 1;
            continue;
        }

        match fs::read_to_string(path) {
            Ok(source) => files.push(ScannedFile {
                path: relative_str,
                language: lang,
                source,
            }),
            Err(_) => stats.skipped_files += 1,
        }
    }

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

/// 解析单个文件：得到语法树、LOC、复杂度指标、git 时间线、依赖数据。
fn parse_file(
    file: &ScannedFile,
    git_metrics: &std::collections::HashMap<String, FileGitMetrics>,
    timelines: &std::collections::HashMap<String, GitTimeline>,
    findings: &[DependencyFinding],
    offline: bool,
    query_failed: bool,
) -> ParsedFile {
    let adapter = adapter_for(file.language);

    let (tree, loc, parse_incomplete, complexity) = if let Some(adapter) = &adapter {
        match adapter.parse(&file.source) {
            Ok(tree) => {
                let loc = rustburn_core::analyzers::complexity::calculate_loc(&tree, &file.source);
                let parse_incomplete = tree.root_node().has_error();
                let complexity = rustburn_core::analyzers::complexity::compute_metrics(
                    &tree,
                    &file.source,
                    adapter.as_ref(),
                );
                (Some(tree), loc, parse_incomplete, complexity)
            }
            Err(_) => {
                let fallback_loc =
                    file.source.lines().filter(|l| !l.trim().is_empty()).count() as u32;
                (
                    None,
                    fallback_loc,
                    true,
                    FileComplexity {
                        loc: fallback_loc,
                        cyclomatic_complexity: 1,
                        max_if_nesting_depth: 0,
                        nested_if_ratio: 0.0,
                        avg_function_length: 0.0,
                        max_function_length: 0,
                        parse_incomplete: true,
                    },
                )
            }
        }
    } else {
        let fallback_loc = file.source.lines().filter(|l| !l.trim().is_empty()).count() as u32;
        (
            None,
            fallback_loc,
            true,
            FileComplexity {
                loc: fallback_loc,
                cyclomatic_complexity: 1,
                max_if_nesting_depth: 0,
                nested_if_ratio: 0.0,
                avg_function_length: 0.0,
                max_function_length: 0,
                parse_incomplete: true,
            },
        )
    };

    let git = timelines.get(&file.path).cloned().unwrap_or_else(|| {
        // 兜底：从聚合指标补一个空时间线（无 commit 数据 → ChangeRiskAnalyzer 标记缺失）
        GitTimeline::default()
    });

    let file_findings: Vec<&DependencyFinding> = findings
        .iter()
        .filter(|f| f.affected_files.contains(&file.path))
        .collect();
    let max_severity = file_findings
        .iter()
        .map(|f| f.severity)
        .max()
        .unwrap_or(Severity::None);

    let dep = DependencyFileData {
        max_cve_severity: max_severity,
        cve_count: file_findings.len() as u32,
        data_incomplete: offline || query_failed,
    };

    let _ = git_metrics; // 聚合指标（commit_count 等）仍写入 FileRawMetrics

    ParsedFile {
        scanned: ScannedFile {
            path: file.path.clone(),
            language: file.language,
            source: file.source.clone(),
        },
        tree,
        loc,
        parse_incomplete,
        complexity,
        git,
        dep,
    }
}

/// 构建单文件分析上下文。
fn build_ctx<'a>(
    parsed: &'a ParsedFile,
    repo: &'a RepoAnalysisData,
    adapter: &'a dyn LanguageAdapter,
) -> FileContext<'a> {
    FileContext {
        path: &parsed.scanned.path,
        source: &parsed.scanned.source,
        language: parsed.scanned.language,
        loc: parsed.loc,
        parse_incomplete: parsed.parse_incomplete,
        tree: parsed.tree.as_ref(),
        adapter,
        git: &parsed.git,
        dependency: &parsed.dep,
        repo,
    }
}

/// 计算均值（空集合返回 None）。
fn mean(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<f64>() / values.len() as f64)
    }
}

/// 执行扫描，返回 (repo_total_heat_score, fail_above)。
fn run() -> Result<(f64, Option<f64>), String> {
    let cli = Cli::parse();

    if let Some(Commands::Update {
        yes,
        api_url,
        dl_base,
    }) = &cli.command
    {
        run_update(*yes, api_url.as_deref(), dl_base.as_deref())?;
        return Ok((0.0, None));
    }

    let path = match &cli.command {
        Some(Commands::Scan { path }) => path.clone(),
        Some(Commands::Update { .. }) => unreachable!("handled above"),
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

    // Phase 1: Git 历史分析（聚合指标 + 时间线）
    let (git_metrics, history_truncated, history_rewrite) =
        analyze_git_history(repo_path, cli.max_commits).map_err(|e| e.to_string())?;
    let (timelines, _, _) =
        analyze_git_timelines(repo_path, cli.max_commits).map_err(|e| e.to_string())?;

    // Phase 2: 扫描文件
    ensure_rbignore(repo_path);
    let (scanned_files, scan_stats) =
        scan_files(repo_path, &cli.ignore, cli.include_generated).map_err(|e| e.to_string())?;

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
    if cli.include_generated {
        warnings.push(
            "--include-generated enabled: generated artifacts (target/, node_modules/, dist/, build/, *.generated.*) are included in the scan."
                .to_string(),
        );
    }
    warnings.push(
        "Trend analysis is not enabled in this version: no historical snapshots are collected, trend_coefficient is fixed at 1.0."
            .to_string(),
    );
    if history_truncated {
        warnings.push(format!(
            "History analysis limited to the latest {} commits.",
            cli.max_commits
        ));
    }
    if scanned_files.len() == 1 {
        warnings.push("单文件仓库：percentile 设为 50".to_string());
    }

    let sample_size_warning = (scanned_files.len() as u32) < cli.min_files;
    if sample_size_warning {
        warnings.push(format!(
            "样本量较小（{} 个文件 < {}）：百分位排名统计噪声较大，请结合绝对阈值分数判断",
            scanned_files.len(),
            cli.min_files
        ));
    }

    // Phase 4: 解析全部文件（每个文件只解析一次，语法树供多个分析器共享）
    let query_failed = dep_analysis.query_status == "query_failed";
    let parsed_files: Vec<ParsedFile> = scanned_files
        .iter()
        .map(|f| {
            parse_file(
                f,
                &git_metrics,
                &timelines,
                &findings,
                cli.offline,
                query_failed,
            )
        })
        .collect();

    // 仓库级数据：复杂度原始值分布
    let mut repo = RepoAnalysisData {
        complexity_raw_values: parsed_files
            .iter()
            .filter(|p| p.tree.is_some())
            .map(|p| complexity_raw_value(&p.complexity))
            .collect(),
        ..Default::default()
    };

    // 覆盖率报告 + 测试注册表
    let coverage_content = rustburn_core::analyzers::test::read_coverage_report(repo_path);
    let test_inputs: Vec<TestFileInput> = parsed_files
        .iter()
        .map(|p| TestFileInput {
            path: p.scanned.path.clone(),
            source: p.scanned.source.clone(),
            language: p.scanned.language,
            loc: p.loc,
        })
        .collect();
    let test_ctx = build_test_context(
        &test_inputs,
        coverage_content.as_deref(),
        &TestPathRules::default(),
    );
    repo.test = test_ctx;

    // Phase 5: 计算各维度仓库均值（DataMissing 填充用）
    let now = Utc::now().timestamp();

    // 复杂度均值
    let mut complexity_risks: Vec<f64> = Vec::new();
    for p in parsed_files.iter().filter(|p| p.tree.is_some()) {
        let raw = complexity_raw_value(&p.complexity);
        let pct = repo_percentile(raw, &repo.complexity_raw_values);
        let abs = absolute_complexity_score(&p.complexity);
        complexity_risks.push((0.5 * pct + 0.5 * abs).clamp(0.0, 100.0));
    }
    repo.complexity_risk_mean = mean(&complexity_risks);

    // 重复度：跨文件结构哈希分组（SPEC v2 §3），再算均值
    let mut adapters: Vec<Box<dyn LanguageAdapter>> = Vec::new();
    for p in parsed_files.iter().filter(|p| p.tree.is_some()) {
        if let Some(adapter) = adapter_for(p.scanned.language) {
            adapters.push(adapter);
        }
    }
    let mut dup_inputs: Vec<DuplicationFileInput<'_>> = Vec::new();
    for (p, adapter) in parsed_files
        .iter()
        .filter(|p| p.tree.is_some())
        .zip(adapters.iter())
    {
        dup_inputs.push(DuplicationFileInput {
            path: &p.scanned.path,
            tree: p.tree.as_ref().expect("filtered by tree.is_some()"),
            source: &p.scanned.source,
            adapter: adapter.as_ref(),
            loc: p.loc,
        });
    }
    let dup_groups = build_duplication_groups(&dup_inputs);
    repo.duplication_line_ranges = dup_groups;

    let mut duplication_risks: Vec<f64> = Vec::new();
    for p in parsed_files.iter().filter(|p| p.tree.is_some()) {
        let ranges = repo
            .duplication_line_ranges
            .get(&p.scanned.path)
            .cloned()
            .unwrap_or_default();
        duplication_risks.push(duplication_risk_from_ranges(&ranges, p.loc));
    }
    repo.duplication_risk_mean = mean(&duplication_risks);

    // 变更风险均值
    let mut change_risks: Vec<f64> = Vec::new();
    for p in &parsed_files {
        if !p.git.is_empty() {
            change_risks.push(change_risk_value(&p.git, now));
        }
    }
    repo.change_risk_mean = mean(&change_risks);

    // 依赖均值
    let mut dependency_risks: Vec<f64> = Vec::new();
    for p in &parsed_files {
        if !p.dep.data_incomplete && !p.parse_incomplete {
            dependency_risks.push(dependency_risk(&p.dep));
        }
    }
    repo.dependency_risk_mean = mean(&dependency_risks);

    // Phase 6: 五个维度分析器
    let analyzers: [Box<dyn DimensionAnalyzer>; 5] = [
        Box::new(ComplexityAnalyzer),
        Box::new(DuplicationAnalyzer),
        Box::new(TestAnalyzer),
        Box::new(ChangeRiskAnalyzer),
        Box::new(DependencyAnalyzer),
    ];

    let mut file_scores: Vec<FileScore> = Vec::new();
    for p in &parsed_files {
        let adapter = adapter_for(p.scanned.language);
        let Some(adapter) = adapter else {
            continue;
        };
        let ctx = build_ctx(p, &repo, adapter.as_ref());

        let mut dims: Vec<DimensionResult> = Vec::with_capacity(5);
        for analyzer in &analyzers {
            dims.push(analyzer.analyze(&ctx));
        }
        let dims_arr: [DimensionResult; 5] =
            dims.try_into().map_err(|_| "维度数错误".to_string())?;

        let composition = calculate_base_risk_score(&dims_arr);
        let base_risk = composition.base_risk_score;

        let consistency = ConsistencyReport {
            coverage_report_stale: false,
            history_rewrite,
            lockfile_mismatch: false,
            coefficient: calculate_consistency_coefficient(false, history_rewrite, false),
        };
        let trend_coefficient = calculate_trend_coefficient(&[]);
        let final_heat = calculate_final_heat_score(base_risk, trend_coefficient);

        let raw = FileRawMetrics {
            path: p.scanned.path.clone(),
            language: p.scanned.language,
            loc: p.loc,
            cyclomatic_complexity: p.complexity.cyclomatic_complexity,
            max_if_nesting_depth: p.complexity.max_if_nesting_depth,
            nested_if_ratio: p.complexity.nested_if_ratio,
            avg_function_length: p.complexity.avg_function_length,
            max_function_length: p.complexity.max_function_length,
            commit_count: git_metrics
                .get(&p.scanned.path)
                .map(|g| g.commit_count)
                .unwrap_or(0),
            distinct_authors: git_metrics
                .get(&p.scanned.path)
                .map(|g| g.distinct_authors)
                .unwrap_or(0),
            last_modified_days_ago: git_metrics
                .get(&p.scanned.path)
                .map(|g| g.last_modified_days_ago)
                .unwrap_or(0),
            incident_commit_count: git_metrics
                .get(&p.scanned.path)
                .map(|g| g.incident_commit_count)
                .unwrap_or(0),
            max_cve_severity: p.dep.max_cve_severity,
            cve_count: p.dep.cve_count,
            dependency_staleness: 0.0,
            dependency_data_incomplete: p.dep.data_incomplete,
            parse_incomplete: p.parse_incomplete,
        };

        if p.parse_incomplete {
            warnings.push(format!("Parse warning: {}", p.scanned.path));
        }

        file_scores.push(FileScore {
            raw,
            dimensions: dims_arr.to_vec(),
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
        schema_version: "2.0".to_string(),
        rustburn_version: env!("CARGO_PKG_VERSION").to_string(),
        analysis_version: 2,
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
            sample_size_warning,
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
    let avg_dim = |idx: usize| {
        report
            .files
            .iter()
            .map(|f| f.dimensions.get(idx).map(|d| d.risk_score).unwrap_or(0.0))
            .sum::<f64>()
            / n
    };

    eprintln!("rustburn v{}", env!("CARGO_PKG_VERSION"));
    eprintln!("Scanning {}", path);
    eprintln!("Files       {}", file_count);
    eprintln!("LOC         {}", total_loc);
    eprintln!("Complexity  {:.1}", avg_dim(0));
    eprintln!("Duplication {:.1}", avg_dim(1));
    eprintln!("Test        {:.1}", avg_dim(2));
    eprintln!("ChangeRisk  {:.1}", avg_dim(3));
    eprintln!("Dependency  {:.1}", avg_dim(4));
    eprintln!("Repository heat score: {:.1} / 100", repo_total);
    eprintln!("Report:");
    eprintln!("  {}", output);

    // scan 完成后做一次静默的新版本检查（不影响 exit code）
    maybe_check_update_async(cli.offline);

    Ok((repo_total, cli.fail_above))
}

/// `rb update`：检查最新版本，用户确认后下载、校验并原子替换。
fn run_update(yes: bool, api_url: Option<&str>, dl_base: Option<&str>) -> Result<(), String> {
    let api_url = api_url.unwrap_or(DEFAULT_API_URL);
    let dl_base = dl_base.unwrap_or(DEFAULT_DL_BASE);

    let release = latest_release(api_url, Duration::from_secs(15))
        .map_err(|e| format!("无法获取最新版本信息: {}", e))?;
    let current = env!("CARGO_PKG_VERSION");

    println!("当前版本: {}", current);
    println!("最新版本: {}", release.tag_name);
    println!("--- release notes 摘要 ---");
    println!("{}", notes_summary(&release.body, 400));

    if !is_newer(&release.tag_name, current) {
        println!("已是最新版本。");
        return Ok(());
    }

    if !yes && !confirm_update(&release.tag_name)? {
        println!("已取消更新。");
        return Ok(());
    }

    let target = platform_target().ok_or_else(|| "当前平台不受支持，无法自动更新。".to_string())?;
    let exe = std::env::current_exe().map_err(|e| format!("无法定位当前可执行文件: {}", e))?;

    update_to_latest(
        dl_base,
        &release.tag_name,
        &target,
        &exe,
        Duration::from_secs(60),
    )
    .map_err(|e| format!("更新失败（原可执行文件未被修改）: {}", e))?;

    println!(
        "已更新到 {}。重新运行 rb --version 确认。",
        release.tag_name
    );
    Ok(())
}

/// 交互确认：输入 y/yes 才继续。
fn confirm_update(version: &str) -> Result<bool, String> {
    use std::io::Write;
    print!("确认升级到 {}？[y/N] ", version);
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let answer = line.trim().to_ascii_lowercase();
    Ok(answer == "y" || answer == "yes")
}

/// scan 结束后的一次性后台更新检查。
fn maybe_check_update_async(offline: bool) {
    if !update_check_enabled(offline) {
        return;
    }
    let Some(cache) = cache_dir() else {
        return;
    };

    let current = env!("CARGO_PKG_VERSION").to_string();
    let current_for_thread = current.clone();
    let cache_thread = cache.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = check_update_silently(
            &cache_thread,
            Duration::from_secs(24 * 3600),
            DEFAULT_API_URL,
            Duration::from_secs(2),
            &current_for_thread,
        );
        let _ = tx.send(result);
    });

    if let Ok(Ok(Some(latest))) = rx.recv_timeout(Duration::from_secs(2)) {
        eprintln!(
            "[rustburn] 发现新版本 {}（当前 {}）。运行 `rb update` 查看并升级。",
            latest, current
        );
    }
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
