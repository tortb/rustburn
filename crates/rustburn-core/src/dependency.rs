//! 依赖分析模块（lockfile 解析 + OSV 查询）。
//!
//! 支持 Cargo.lock 和 package-lock.json 解析，以及 OSV API 批量查询。

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::lang::{profile_for, LockfileParser};
use crate::model::{DependencyFinding, Language, Severity};

#[derive(Error, Debug)]
pub enum DependencyError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Parse error: {0}")]
    Parse(String),

    #[error("HTTP error: {0}")]
    Http(String),
}

/// 依赖包信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dependency {
    pub name: String,
    pub version: String,
    pub ecosystem: String,
}

/// 依赖分析结果
#[derive(Debug, Clone)]
pub struct DependencyAnalysis {
    pub dependencies: Vec<Dependency>,
    pub findings: Vec<DependencyFinding>,
    pub query_status: String,
}

/// go.sum 锁文件解析器（`module version hash` 三元组格式）。
///
/// OSV 查询时 ecosystem 使用 `Go`；OSV 的 Go 版本号不带 `v` 前缀
/// （如 `1.21.5`），解析时去掉。`module version/go.mod` 行是模块根的
/// go.mod 校验专用行，与同版本主行去重合并。
pub struct GoSumLockfileParser;

impl LockfileParser for GoSumLockfileParser {
    fn name(&self) -> &'static str {
        "go.sum"
    }

    fn lockfile_names(&self) -> &'static [&'static str] {
        &["go.sum"]
    }

    fn parse(&self, content: &str) -> Vec<Dependency> {
        use std::collections::HashSet;
        let mut seen = HashSet::new();
        let mut deps = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with("//") {
                continue;
            }
            let mut fields = line.split_whitespace();
            let (Some(module), Some(version)) = (fields.next(), fields.next()) else {
                continue;
            };
            let version = version.strip_suffix("/go.mod").unwrap_or(version);
            let version = version.strip_prefix('v').unwrap_or(version);
            if seen.insert((module.to_string(), version.to_string())) {
                deps.push(Dependency {
                    name: module.to_string(),
                    version: version.to_string(),
                    ecosystem: "Go".to_string(),
                });
            }
        }
        deps
    }
}

/// 解析 Cargo.lock 文件
pub fn parse_cargo_lock(path: &Path) -> Result<Vec<Dependency>, DependencyError> {
    let content = fs::read_to_string(path)?;
    let mut deps = Vec::new();

    let mut current_name = None;
    let mut current_version = None;

    for line in content.lines() {
        let line = line.trim();

        if line == "[[package]]" {
            // 保存上一个包
            if let (Some(name), Some(version)) = (current_name.take(), current_version.take()) {
                deps.push(Dependency {
                    name,
                    version,
                    ecosystem: "crates.io".to_string(),
                });
            }
        } else if let Some(name_str) = line.strip_prefix("name = ") {
            current_name = Some(name_str.trim_matches('"').to_string());
        } else if let Some(version_str) = line.strip_prefix("version = ") {
            current_version = Some(version_str.trim_matches('"').to_string());
        }
    }

    // 保存最后一个包
    if let (Some(name), Some(version)) = (current_name, current_version) {
        deps.push(Dependency {
            name,
            version,
            ecosystem: "crates.io".to_string(),
        });
    }

    Ok(deps)
}

/// 解析 package-lock.json 文件
pub fn parse_package_lock(path: &Path) -> Result<Vec<Dependency>, DependencyError> {
    let content = fs::read_to_string(path)?;
    let json: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| DependencyError::Parse(e.to_string()))?;

    let mut deps = Vec::new();
    let lockfile_version = json
        .get("lockfileVersion")
        .and_then(|v| v.as_i64())
        .unwrap_or(1);

    if lockfile_version >= 2 {
        // v2/v3: 使用 packages 字段
        if let Some(packages) = json.get("packages").and_then(|p| p.as_object()) {
            for (pkg_path, pkg_info) in packages {
                // 跳过根包
                if pkg_path.is_empty() {
                    continue;
                }

                // 提取包名（从路径中）
                let name = pkg_path.strip_prefix("node_modules/").unwrap_or(pkg_path);

                // 处理 scoped packages
                let name = if name.contains("node_modules/") {
                    // 嵌套依赖，取最后一部分
                    name.split("node_modules/").last().unwrap_or(name)
                } else {
                    name
                };

                if let Some(version) = pkg_info.get("version").and_then(|v| v.as_str()) {
                    deps.push(Dependency {
                        name: name.to_string(),
                        version: version.to_string(),
                        ecosystem: "npm".to_string(),
                    });
                }
            }
        }
    } else {
        // v1: 使用 dependencies 字段
        if let Some(dependencies) = json.get("dependencies").and_then(|d| d.as_object()) {
            parse_npm_dependencies(dependencies, &mut deps);
        }
    }

    Ok(deps)
}

/// 递归解析 npm dependencies
fn parse_npm_dependencies(
    deps: &serde_json::Map<String, serde_json::Value>,
    result: &mut Vec<Dependency>,
) {
    for (name, info) in deps {
        if let Some(version) = info.get("version").and_then(|v| v.as_str()) {
            result.push(Dependency {
                name: name.clone(),
                version: version.to_string(),
                ecosystem: "npm".to_string(),
            });
        }

        // 递归处理嵌套依赖
        if let Some(nested) = info.get("dependencies").and_then(|d| d.as_object()) {
            parse_npm_dependencies(nested, result);
        }
    }
}

/// OSV API 请求结构
#[derive(Debug, Serialize)]
struct OsvRequest {
    queries: Vec<OsvQuery>,
}

#[derive(Debug, Serialize)]
struct OsvQuery {
    package: OsvPackage,
    version: String,
}

#[derive(Debug, Serialize)]
struct OsvPackage {
    name: String,
    ecosystem: String,
}

/// OSV API 响应结构
#[derive(Debug, Deserialize)]
struct OsvResponse {
    /// 与查询一一对应的结果；单条查询失败时 OSV 可能返回 null
    results: Vec<Option<OsvResult>>,
}

#[derive(Debug, Deserialize)]
struct OsvResult {
    vulns: Option<Vec<OsvVuln>>,
}

#[derive(Debug, Deserialize)]
struct OsvVuln {
    id: String,
    summary: Option<String>,
    database_specific: Option<serde_json::Value>,
}

/// 查询 OSV API（默认官方端点）。
pub fn query_osv(deps: &[Dependency]) -> Result<Vec<DependencyFinding>, DependencyError> {
    query_osv_with_base(deps, "https://api.osv.dev")
}

/// 查询 OSV API（可指定 base URL，供测试 mock 使用）。
pub fn query_osv_with_base(
    deps: &[Dependency],
    base_url: &str,
) -> Result<Vec<DependencyFinding>, DependencyError> {
    if deps.is_empty() {
        return Ok(Vec::new());
    }

    // 构建批量查询
    let queries: Vec<OsvQuery> = deps
        .iter()
        .map(|d| OsvQuery {
            package: OsvPackage {
                name: d.name.clone(),
                ecosystem: d.ecosystem.clone(),
            },
            version: d.version.clone(),
        })
        .collect();

    let request = OsvRequest { queries };

    // 发送 HTTP 请求
    let client = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(10))
        .build();

    let mut attempts = 0;
    let max_attempts = 2;

    loop {
        attempts += 1;

        let json_body =
            serde_json::to_string(&request).map_err(|e| DependencyError::Parse(e.to_string()))?;

        let response = client
            .post(&format!("{}/v1/querybatch", base_url))
            .set("Content-Type", "application/json")
            .send_string(&json_body);

        match response {
            Ok(resp) => {
                let body = resp
                    .into_string()
                    .map_err(|e| DependencyError::Http(e.to_string()))?;
                let osv_response: OsvResponse = serde_json::from_str(&body)
                    .map_err(|e| DependencyError::Parse(e.to_string()))?;

                let mut findings = process_osv_response(&osv_response, deps);

                if crate::debug_enabled() {
                    crate::debug_log(format_args!(
                        "osv_querybatch status=success queries={} response_bytes={} raw_findings={}",
                        request.queries.len(),
                        body.len(),
                        findings.len()
                    ));
                }

                enrich_findings(&mut findings, base_url);
                return Ok(findings);
            }
            Err(e) => {
                if crate::debug_enabled() {
                    crate::debug_log(format_args!(
                        "osv_querybatch attempt={}/{} failed: {}",
                        attempts, max_attempts, e
                    ));
                }
                if attempts >= max_attempts {
                    return Err(DependencyError::Http(e.to_string()));
                }
                // 重试
                continue;
            }
        }
    }
}

/// OSV 单条漏洞详情响应（GET /v1/vulns/{id}）。
///
/// querybatch 接口出于体积考虑**只返回 id 与 modified**，
/// summary / 详情 / 严重度必须通过单条接口二次获取。
#[derive(Debug, Deserialize)]
struct OsvVulnDetail {
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    database_specific: Option<serde_json::Value>,
    /// 同一漏洞在其他编号体系中的 ID（如 GHSA <-> RUSTSEC）
    #[serde(default)]
    aliases: Vec<String>,
}

/// 补全漏洞摘要与真实严重度。
///
/// 对批量查询结果中每个去重后的漏洞 id 再发一次
/// `GET {base_url}/v1/vulns/{id}`，用官方返回的 summary 填充报告，
/// 避免报告中出现 ID 真实但摘要为空的记录。
/// 单个详情请求失败时保留批量结果（summary 为空、严重度标记为估算）。
fn enrich_findings(findings: &mut Vec<DependencyFinding>, base_url: &str) {
    use std::collections::{HashMap, HashSet};

    if findings.is_empty() {
        return;
    }

    let client = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(10))
        .build();

    // 收集去重后的漏洞 id
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for f in findings.iter() {
        if seen.insert(f.id.clone()) {
            ids.push(f.id.clone());
        }
    }

    let mut details: HashMap<String, OsvVulnDetail> = HashMap::new();
    let mut fetched_ok = 0usize;
    let mut fetched_fail = 0usize;
    for id in &ids {
        match client.get(&format!("{}/v1/vulns/{}", base_url, id)).call() {
            Ok(resp) => match resp.into_string() {
                Ok(body) => {
                    if let Ok(detail) = serde_json::from_str::<OsvVulnDetail>(&body) {
                        details.insert(id.to_string(), detail);
                        fetched_ok += 1;
                    } else {
                        fetched_fail += 1;
                    }
                }
                Err(_) => fetched_fail += 1,
            },
            Err(_) => fetched_fail += 1,
        }
    }

    if crate::debug_enabled() {
        crate::debug_log(format_args!(
            "osv_vuln_details unique_ids={} fetched_ok={} fetched_fail={}",
            ids.len(),
            fetched_ok,
            fetched_fail
        ));
    }

    for f in findings.iter_mut() {
        let Some(detail) = details.get(&f.id) else {
            continue;
        };

        // 填充真实摘要（批量接口不返回）
        if let Some(summary) = &detail.summary {
            if !summary.trim().is_empty() {
                f.summary = summary.clone();
            }
        }

        // 用真实严重度覆盖估算值（无 CVSS / severity 时保持估算标记）
        if let Some((severity, estimated)) = compute_detail_severity(&detail.database_specific) {
            f.severity = severity;
            f.severity_estimated = estimated;
        }

        if crate::debug_enabled() {
            crate::debug_log(format_args!(
                "osv_vuln_enriched id={} summary_len={} severity={} severity_estimated={}",
                f.id,
                f.summary.len(),
                f.severity,
                f.severity_estimated
            ));
        }
    }

    // 跨数据源去重：同一漏洞可能同时以 GHSA 与 RUSTSEC 两套编号出现
    // （详情中的 aliases 互指），必须合并为一条，避免重复计数。
    dedupe_by_aliases(findings, &details);
}

/// 通过 aliases 合并同一漏洞的多条编号记录。
///
/// - canonical 取 `id ∪ aliases` 中字典序最小者；
/// - 同一 canonical 组内只保留一条，优先保留 `RUSTSEC-` 前缀（RustSec 为
///   Rust 生态的权威编号体系），否则保留第一条；
/// - 被合并的记录中更完整的信息（真实严重度 / 非空摘要）会吸收到保留记录上；
/// - 详情拉取失败（无 aliases）的记录按自身 ID 独立成组，不受影响。
fn dedupe_by_aliases(
    findings: &mut Vec<DependencyFinding>,
    details: &std::collections::HashMap<String, OsvVulnDetail>,
) {
    use std::collections::{HashMap as Map, HashSet};

    if findings.len() <= 1 {
        return;
    }

    let ids: Vec<String> = findings.iter().map(|f| f.id.clone()).collect();

    // 计算每个 id 的 canonical（组键）
    let mut canonical_of: Map<String, String> = Map::new();
    for id in &ids {
        let mut keys = vec![id.clone()];
        if let Some(detail) = details.get(id) {
            keys.extend(detail.aliases.iter().cloned());
        }
        let canonical = keys.iter().min().cloned().unwrap_or_else(|| id.clone());
        canonical_of.insert(id.clone(), canonical);
    }

    // 组内选择保留的记录
    let mut groups: Map<String, Vec<String>> = Map::new();
    for id in &ids {
        groups
            .entry(canonical_of[id].clone())
            .or_default()
            .push(id.clone());
    }

    let chosen_list: Vec<(String, Vec<String>)> = groups
        .into_values()
        .map(|members| {
            let chosen = members
                .iter()
                .find(|m| m.starts_with("RUSTSEC-"))
                .unwrap_or(&members[0])
                .clone();
            (chosen, members)
        })
        .collect();

    let keep: HashSet<String> = chosen_list.iter().map(|(c, _)| c.clone()).collect();
    let before = findings.len();
    findings.retain(|f| keep.contains(&f.id));

    // 从被合并的记录中吸收更完整的信息（如 GHSA 侧的真实严重度）
    for (chosen, members) in chosen_list {
        let Some(target) = findings.iter_mut().find(|f| f.id == chosen) else {
            continue;
        };
        for id in members {
            if id == chosen {
                continue;
            }
            let Some(detail) = details.get(&id) else {
                continue;
            };
            if target.severity_estimated {
                if let Some((severity, estimated)) =
                    compute_detail_severity(&detail.database_specific)
                {
                    if !estimated {
                        target.severity = severity;
                        target.severity_estimated = false;
                    }
                }
            }
            if target.summary.is_empty() {
                if let Some(summary) = &detail.summary {
                    if !summary.trim().is_empty() {
                        target.summary = summary.clone();
                    }
                }
            }
        }
    }

    if crate::debug_enabled() {
        crate::debug_log(format_args!(
            "osv_vuln_dedupe before={} after={}",
            before,
            findings.len()
        ));
    }
}

/// 从漏洞详情中计算严重度。
///
/// 优先使用 CVSS 分数；其次使用 GitHub advisory 的
/// `database_specific.severity`（LOW/MEDIUM/HIGH/CRITICAL）。
fn compute_detail_severity(
    database_specific: &Option<serde_json::Value>,
) -> Option<(Severity, bool)> {
    if let Some(score) = extract_cvss_score(database_specific) {
        return Some(crate::model::cvss_to_severity(Some(score)));
    }

    let db = database_specific.as_ref()?;
    let level = db.get("severity").and_then(|v| v.as_str())?;
    let severity = match level.to_ascii_uppercase().as_str() {
        "LOW" => Severity::Low,
        "MEDIUM" => Severity::Medium,
        "HIGH" => Severity::High,
        "CRITICAL" => Severity::Critical,
        _ => return None,
    };
    Some((severity, false))
}

/// 处理 OSV 响应
fn process_osv_response(response: &OsvResponse, deps: &[Dependency]) -> Vec<DependencyFinding> {
    let mut findings = Vec::new();

    for (i, result) in response.results.iter().enumerate() {
        // 跳过 OSV 对单条查询失败返回的 null 结果，以及结果数多于查询数的异常情况
        let (Some(result), Some(dep)) = (result.as_ref(), deps.get(i)) else {
            continue;
        };

        if let Some(vulns) = &result.vulns {
            for vuln in vulns {
                // 提取 CVSS 分数
                let cvss_score = extract_cvss_score(&vuln.database_specific);
                let (severity, severity_estimated) = crate::model::cvss_to_severity(cvss_score);

                findings.push(DependencyFinding {
                    id: vuln.id.clone(),
                    package_name: dep.name.clone(),
                    ecosystem: dep.ecosystem.clone(),
                    version: dep.version.clone(),
                    severity,
                    severity_estimated,
                    summary: vuln.summary.clone().unwrap_or_default(),
                    affected_files: Vec::new(), // 后续填充
                });
            }
        }
    }

    findings
}

/// 从 database_specific 提取 CVSS 分数
fn extract_cvss_score(database_specific: &Option<serde_json::Value>) -> Option<f64> {
    let db = database_specific.as_ref()?;

    // 优先尝试直接获取 score 字段
    if let Some(score) = db.get("score").and_then(|s| s.as_f64()) {
        return Some(score);
    }

    extract_cvss_vector_score(db)
}

/// 从 `cvss` 字段提取分数：数字 / 括号内分数 / CVSS v3 向量。
fn extract_cvss_vector_score(db: &serde_json::Value) -> Option<f64> {
    let cvss = db.get("cvss")?;
    if let Some(score) = cvss.as_f64() {
        return Some(score);
    }
    let cvss_str = cvss.as_str()?;

    parse_parenthesized_score(cvss_str).or_else(|| {
        // 尝试从向量字符串计算分数
        if cvss_str.starts_with("CVSS:3") {
            Some(calculate_cvss_v3_score(cvss_str))
        } else {
            None
        }
    })
}

/// 解析向量后附带的分数，例如 `"CVSS:3.1/AV:N/... (9.8)"`。
fn parse_parenthesized_score(cvss_str: &str) -> Option<f64> {
    let paren_start = cvss_str.rfind('(')?;
    let paren_end = cvss_str.rfind(')')?;
    if paren_start >= paren_end {
        return None;
    }
    cvss_str[paren_start + 1..paren_end].trim().parse().ok()
}

/// 从 CVSS v3 向量字符串计算分数
/// 这是一个简化的实现，基于关键指标估算分数
fn calculate_cvss_v3_score(vector: &str) -> f64 {
    compute_cvss_v3_score(&parse_cvss_v3_vector(vector))
}

/// CVSS v3 向量解析出的关键指标。
struct CvssV3 {
    attack_vector: f64,
    attack_complexity: f64,
    privileges_required: f64,
    user_interaction: f64,
    scope_changed: bool,
    confidentiality: f64,
    integrity: f64,
    availability: f64,
}

impl Default for CvssV3 {
    fn default() -> Self {
        Self {
            attack_vector: 0.0,
            attack_complexity: 0.0,
            privileges_required: 0.0,
            user_interaction: 0.0,
            scope_changed: false,
            confidentiality: 0.0,
            integrity: 0.0,
            availability: 0.0,
        }
    }
}

/// 解析 CVSS v3 向量字符串为指标值。
fn parse_cvss_v3_vector(vector: &str) -> CvssV3 {
    let mut cvss = CvssV3::default();

    for part in vector.split('/') {
        let part = part.trim();
        let Some((key, value)) = part.split_once(':') else {
            continue;
        };

        match key {
            "AV" => {
                cvss.attack_vector = metric_value(
                    value,
                    &[("N", 0.85), ("A", 0.62), ("L", 0.55), ("P", 0.20)],
                    0.5,
                )
            }
            "AC" => cvss.attack_complexity = metric_value(value, &[("L", 0.77), ("H", 0.44)], 0.5),
            "PR" => {
                cvss.privileges_required =
                    metric_value(value, &[("N", 0.85), ("L", 0.62), ("H", 0.27)], 0.5)
            }
            "UI" => cvss.user_interaction = metric_value(value, &[("N", 0.85), ("R", 0.62)], 0.5),
            "S" => cvss.scope_changed = value == "C", // Changed
            "C" => {
                cvss.confidentiality =
                    metric_value(value, &[("H", 0.56), ("L", 0.22), ("N", 0.0)], 0.2)
            }
            "I" => {
                cvss.integrity = metric_value(value, &[("H", 0.56), ("L", 0.22), ("N", 0.0)], 0.2)
            }
            "A" => {
                cvss.availability =
                    metric_value(value, &[("H", 0.56), ("L", 0.22), ("N", 0.0)], 0.2)
            }
            _ => {}
        }
    }

    cvss
}

/// 按映射表查取指标值，未知取值回退到默认值。
fn metric_value(value: &str, table: &[(&str, f64)], default: f64) -> f64 {
    table
        .iter()
        .find(|(k, _)| *k == value)
        .map(|(_, v)| *v)
        .unwrap_or(default)
}

/// 根据 CVSS v3 指标计算基础分数（四舍五入到一位小数）。
fn compute_cvss_v3_score(cvss: &CvssV3) -> f64 {
    // 计算 Impact Sub Score (ISS)
    let iss: f64 =
        1.0 - ((1.0 - cvss.confidentiality) * (1.0 - cvss.integrity) * (1.0 - cvss.availability));

    // 计算 Impact
    let impact = if cvss.scope_changed {
        7.52 * (iss - 0.029) - 3.25 * (iss - 0.02).powf(15.0)
    } else {
        6.42 * iss
    };

    // 计算 Exploitability
    let exploitability = 8.22
        * cvss.attack_vector
        * cvss.attack_complexity
        * cvss.privileges_required
        * cvss.user_interaction;

    // 计算基础分数
    let base_score = if impact <= 0.0 {
        0.0
    } else if cvss.scope_changed {
        (1.25 * (impact + exploitability) - 0.7).min(10.0)
    } else {
        (impact + exploitability).min(10.0)
    };

    // 四舍五入到一位小数
    (base_score * 10.0).round() / 10.0
}

/// 从源文件提取依赖引用
pub fn extract_imports_from_source(path: &Path, source: &str) -> Vec<String> {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "rs" => extract_rust_imports(source),
        "js" | "jsx" => extract_js_imports(source),
        "go" => extract_go_imports(source),
        _ => Vec::new(),
    }
}

/// 解析 Go 源码中的 import 引用（含 `import (...) {}` 块与单行 `import "x"`，
/// 以及 `alias "path"` 形式）。返回模块路径，与 go.sum 的 module 字段对齐。
fn extract_go_imports(source: &str) -> Vec<String> {
    let mut imports = Vec::new();
    let mut in_block = false;
    for line in source.lines() {
        let line = line.trim();
        if line.starts_with("import") {
            in_block = in_block || line.contains('(');
        } else if in_block {
            if line.contains(')') {
                in_block = false;
                continue;
            }
        } else {
            continue;
        }
        // 提取引号包裹的模块路径（import "fmt" / gin "github.com/gin-gonic/gin"）
        if let Some(start) = line.find('"') {
            if let Some(end) = line[start + 1..].find('"') {
                let module = &line[start + 1..start + 1 + end];
                if !module.is_empty() && !imports.iter().any(|m| m == module) {
                    imports.push(module.to_string());
                }
            }
        }
    }
    imports
}

/// 解析 Rust 源码中的 use / extern crate 引用。
fn extract_rust_imports(source: &str) -> Vec<String> {
    let mut imports = Vec::new();

    for line in source.lines() {
        let line = line.trim();

        if line.starts_with("use ") {
            // 提取 crate 名称
            if let Some(crate_name) = line.strip_prefix("use ").and_then(|s| s.split("::").next()) {
                let crate_name = crate_name.trim();
                if !crate_name.is_empty()
                    && crate_name != "super"
                    && crate_name != "self"
                    && crate_name != "crate"
                {
                    imports.push(crate_name.to_string());
                }
            }
        } else if line.starts_with("extern crate ") {
            if let Some(crate_name) = line
                .strip_prefix("extern crate ")
                .and_then(|s| s.split(';').next())
            {
                let crate_name = crate_name.trim();
                if !crate_name.is_empty() {
                    imports.push(crate_name.to_string());
                }
            }
        }
    }

    imports
}

/// 解析 JavaScript 源码中的 import / require 引用。
fn extract_js_imports(source: &str) -> Vec<String> {
    let mut imports = Vec::new();

    for line in source.lines() {
        let line = line.trim();

        // import ... from '...'
        if line.starts_with("import ") && line.contains(" from ") {
            if let Some(module) = line.split(" from ").nth(1) {
                let module = module
                    .trim()
                    .trim_end_matches(';')
                    .trim_matches(|c| c == '\'' || c == '"');
                push_npm_import(&mut imports, module);
            }
        }
        // require('...')
        else if line.contains("require(") {
            if let Some(start) = line.find("require(") {
                let after = &line[start + 8..];
                if let Some(end) = after.find(')') {
                    let module = after[..end].trim().trim_matches(|c| c == '\'' || c == '"');
                    push_npm_import(&mut imports, module);
                }
            }
        }
    }

    imports
}

/// 跳过相对路径，归一化后加入 imports。
fn push_npm_import(imports: &mut Vec<String>, module: &str) {
    // 跳过相对路径
    if !module.starts_with('.') {
        imports.push(normalize_npm_module(module));
    }
}

/// 归一化 npm 模块名：scoped 包保留 @scope/name，其余只取第一段。
fn normalize_npm_module(module: &str) -> String {
    if module.starts_with('@') {
        module.split('/').take(2).collect::<Vec<_>>().join("/")
    } else {
        module.split('/').next().unwrap_or(module).to_string()
    }
}

/// 将 Cargo crate 名称转换为 Rust import 名称
pub fn cargo_to_rust_import(crate_name: &str) -> String {
    // Cargo 中 foo-bar 在 Rust 中变为 foo_bar
    crate_name.replace('-', "_")
}

/// 完整的依赖分析
pub fn analyze_dependencies(
    repo_path: &Path,
    offline: bool,
) -> Result<DependencyAnalysis, DependencyError> {
    let mut all_deps = Vec::new();

    // 解析 Cargo.lock
    let cargo_lock_path = repo_path.join("Cargo.lock");
    if cargo_lock_path.exists() {
        let cargo_deps = parse_cargo_lock(&cargo_lock_path)?;
        all_deps.extend(cargo_deps);
    }

    // 解析 package-lock.json
    let package_lock_path = repo_path.join("package-lock.json");
    if package_lock_path.exists() {
        let npm_deps = parse_package_lock(&package_lock_path)?;
        all_deps.extend(npm_deps);
    }

    // 按语言 profile 注册的锁文件解析器（Go 的 go.sum 等）。
    // 语言特定知识（锁文件名、格式、OSV ecosystem）全部收敛在 profile 表，
    // 这里只做通用的"按注册表驱动"遍历。
    for lang in [
        Language::Rust,
        Language::JavaScript,
        Language::Go,
        Language::Mock,
        Language::Unknown,
    ] {
        let Some(profile) = profile_for(lang) else {
            continue;
        };
        for parser in profile.lockfile_parsers {
            for name in parser.lockfile_names() {
                let path = repo_path.join(name);
                if path.is_file() {
                    if let Ok(content) = fs::read_to_string(&path) {
                        all_deps.extend(parser.parse(&content));
                    }
                }
            }
        }
    }

    // 查询 OSV
    let (findings, query_status) = if offline {
        (Vec::new(), "offline".to_string())
    } else if all_deps.is_empty() {
        (Vec::new(), "no_dependencies".to_string())
    } else {
        match query_osv(&all_deps) {
            Ok(findings) => (findings, "success".to_string()),
            Err(_) => (Vec::new(), "query_failed".to_string()),
        }
    };

    Ok(DependencyAnalysis {
        dependencies: all_deps,
        findings,
        query_status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_parse_cargo_lock() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join("Cargo.lock");
        fs::write(
            &lock_path,
            r#"[[package]]
name = "serde"
version = "1.0.130"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "anyhow"
version = "1.0.44"
"#,
        )
        .unwrap();

        let deps = parse_cargo_lock(&lock_path).unwrap();
        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0].name, "serde");
        assert_eq!(deps[0].version, "1.0.130");
        assert_eq!(deps[0].ecosystem, "crates.io");
    }

    #[test]
    fn test_parse_package_lock_v2() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join("package-lock.json");
        fs::write(
            &lock_path,
            r#"{
  "lockfileVersion": 2,
  "packages": {
    "": {
      "name": "test",
      "version": "1.0.0"
    },
    "node_modules/lodash": {
      "version": "4.17.21"
    },
    "node_modules/@types/node": {
      "version": "16.0.0"
    }
  }
}"#,
        )
        .unwrap();

        let deps = parse_package_lock(&lock_path).unwrap();
        assert_eq!(deps.len(), 2);
        assert!(deps
            .iter()
            .any(|d| d.name == "lodash" && d.version == "4.17.21"));
        assert!(deps
            .iter()
            .any(|d| d.name == "@types/node" && d.version == "16.0.0"));
    }

    #[test]
    fn test_parse_package_lock_v1() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join("package-lock.json");
        fs::write(
            &lock_path,
            r#"{
  "lockfileVersion": 1,
  "dependencies": {
    "lodash": {
      "version": "4.17.21"
    },
    "express": {
      "version": "4.17.1"
    }
  }
}"#,
        )
        .unwrap();

        let deps = parse_package_lock(&lock_path).unwrap();
        assert_eq!(deps.len(), 2);
    }

    #[test]
    fn test_parse_go_sum() {
        let go_sum = r#"golang.org/x/net v0.0.0-20210716203947-853a461950ff h1:...
golang.org/x/net v0.0.0-20210716203947-853a461950ff/go.mod h1:...
github.com/gin-gonic/gin v1.9.0 h1:9A7PoREQDLoWbo0KJmi1MavEhI9FmiKFZ/7RwvC0zPc=
github.com/stretchr/testify v1.8.4 h1:abc
"#;
        let deps = GoSumLockfileParser.parse(go_sum);
        // 两条 golang.org/x/net（含 /go.mod 行）去重合并为 1 条
        assert_eq!(deps.len(), 3, "go.sum 应解析出 3 个唯一 (module, version)");
        let xnet = deps
            .iter()
            .find(|d| d.name == "golang.org/x/net")
            .expect("应解析出 golang.org/x/net");
        assert_eq!(xnet.version, "0.0.0-20210716203947-853a461950ff");
        assert_eq!(xnet.ecosystem, "Go");
        let gin = deps
            .iter()
            .find(|d| d.name == "github.com/gin-gonic/gin")
            .expect("应解析出 github.com/gin-gonic/gin");
        // v 前缀被去掉（OSV Go ecosystem 使用不带 v 的版本号）
        assert_eq!(gin.version, "1.9.0");
        assert_eq!(gin.ecosystem, "Go");
        assert!(deps
            .iter()
            .all(|d| d.ecosystem == "Go" && !d.version.starts_with('v')));
    }

    #[test]
    fn test_extract_go_imports() {
        let source = r#"
package main

import (
    "fmt"
    gin "github.com/gin-gonic/gin"
    "github.com/stretchr/testify/assert"
)

func main() { fmt.Println("hi") }
"#;
        let imports = extract_imports_from_source(Path::new("main.go"), source);
        assert!(imports.contains(&"fmt".to_string()));
        assert!(imports.contains(&"github.com/gin-gonic/gin".to_string()));
        assert!(imports.contains(&"github.com/stretchr/testify/assert".to_string()));
        assert_eq!(imports.len(), 3);
    }

    #[test]
    fn test_extract_imports_rust() {
        let source = r#"
use serde::Deserialize;
use anyhow::Result;
use std::collections::HashMap;

extern crate regex;

fn main() {}
"#;
        let imports = extract_imports_from_source(Path::new("main.rs"), source);
        assert!(imports.contains(&"serde".to_string()));
        assert!(imports.contains(&"anyhow".to_string()));
        assert!(imports.contains(&"std".to_string()));
        assert!(imports.contains(&"regex".to_string()));
    }

    #[test]
    fn test_extract_imports_javascript() {
        let source = r#"
import React from 'react';
import { useState } from 'react';
import lodash from 'lodash';
import { something } from '@scope/package';
const express = require('express');
const local = require('./local');
"#;
        let imports = extract_imports_from_source(Path::new("app.js"), source);
        assert!(imports.contains(&"react".to_string()));
        assert!(imports.contains(&"lodash".to_string()));
        assert!(imports.contains(&"@scope/package".to_string()));
        assert!(imports.contains(&"express".to_string()));
        assert!(!imports.contains(&"local".to_string())); // 相对路径应该被跳过
    }

    #[test]
    fn test_cargo_to_rust_import() {
        assert_eq!(cargo_to_rust_import("serde-json"), "serde_json");
        assert_eq!(cargo_to_rust_import("anyhow"), "anyhow");
    }

    #[test]
    fn test_extract_cvss_score() {
        let db = Some(serde_json::json!({
            "cvss": "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H"
        }));
        let score = extract_cvss_score(&db);
        // 简化实现可能无法正确解析，但至少不应该 panic
        let _ = score;
    }

    #[test]
    fn test_process_osv_response_skips_null_and_missing() {
        // 构造含 null 结果、结果数少于查询数、summary 为空的响应
        let response = OsvResponse {
            results: vec![
                None,
                Some(OsvResult {
                    vulns: Some(vec![OsvVuln {
                        id: "RUSTSEC-0000-0001".to_string(),
                        summary: Some("real summary".to_string()),
                        database_specific: None,
                    }]),
                }),
            ],
        };
        let deps = vec![
            Dependency {
                name: "pkg-a".to_string(),
                version: "1.0.0".to_string(),
                ecosystem: "crates.io".to_string(),
            },
            Dependency {
                name: "pkg-b".to_string(),
                version: "2.0.0".to_string(),
                ecosystem: "crates.io".to_string(),
            },
        ];

        let findings = process_osv_response(&response, &deps);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].id, "RUSTSEC-0000-0001");
        assert_eq!(findings[0].package_name, "pkg-b"); // null 结果索引被跳过，对应第二个依赖
        assert_eq!(findings[0].summary, "real summary");
    }

    #[test]
    fn test_process_osv_response_no_vulns() {
        // vulns 为 None 或空数组 → 不产生 findings
        let response = OsvResponse {
            results: vec![
                Some(OsvResult { vulns: None }),
                Some(OsvResult {
                    vulns: Some(vec![]),
                }),
            ],
        };
        let deps = vec![
            Dependency {
                name: "a".to_string(),
                version: "1".to_string(),
                ecosystem: "crates.io".to_string(),
            },
            Dependency {
                name: "b".to_string(),
                version: "2".to_string(),
                ecosystem: "crates.io".to_string(),
            },
        ];
        assert!(process_osv_response(&response, &deps).is_empty());
    }

    #[test]
    fn test_process_osv_response_results_exceed_deps() {
        // 结果数多于依赖数 → 越界保护，不 panic，只处理有对应依赖的结果
        let response = OsvResponse {
            results: vec![
                Some(OsvResult {
                    vulns: Some(vec![OsvVuln {
                        id: "RUSTSEC-0001".to_string(),
                        summary: None,
                        database_specific: None,
                    }]),
                }),
                Some(OsvResult {
                    vulns: Some(vec![OsvVuln {
                        id: "RUSTSEC-0002".to_string(),
                        summary: None,
                        database_specific: None,
                    }]),
                }),
                Some(OsvResult {
                    vulns: Some(vec![OsvVuln {
                        id: "RUSTSEC-0003".to_string(),
                        summary: None,
                        database_specific: None,
                    }]),
                }),
            ],
        };
        let deps = vec![Dependency {
            name: "a".to_string(),
            version: "1".to_string(),
            ecosystem: "crates.io".to_string(),
        }];
        let findings = process_osv_response(&response, &deps);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].id, "RUSTSEC-0001");
    }

    #[test]
    fn test_compute_detail_severity_from_github_level() {
        // GitHub advisory 的 database_specific.severity 应映射为真实严重度（非估算）
        let db = Some(serde_json::json!({ "severity": "LOW", "cwe_ids": ["CWE-476"] }));
        let (severity, estimated) = compute_detail_severity(&db).unwrap();
        assert_eq!(severity, crate::model::Severity::Low);
        assert!(!estimated, "真实严重度不应标记为估算");

        // 无任何严重度信息 → 返回 None，由上层保持估算 Medium
        assert!(compute_detail_severity(&None).is_none());
    }

    #[test]
    fn test_compute_detail_severity_levels_and_unknown() {
        // 各级 GitHub severity 映射
        for (level, expected) in [
            ("LOW", crate::model::Severity::Low),
            ("medium", crate::model::Severity::Medium),
            ("HIGH", crate::model::Severity::High),
            ("Critical", crate::model::Severity::Critical),
        ] {
            let db = Some(serde_json::json!({ "severity": level }));
            let (severity, estimated) = compute_detail_severity(&db).unwrap();
            assert_eq!(severity, expected, "level={}", level);
            assert!(!estimated);
        }

        // 未知 severity 字符串 → None（保持估算，不 panic）
        assert!(
            compute_detail_severity(&Some(serde_json::json!({ "severity": "INFO" }))).is_none()
        );
        // database_specific 存在但无 severity 字段 → None
        assert!(
            compute_detail_severity(&Some(serde_json::json!({ "license": "CC0-1.0" }))).is_none()
        );
    }

    #[test]
    fn test_extract_cvss_score_v3_vector_score() {
        // CVSS v3.1 向量 AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H → ≈9.8
        let db = Some(serde_json::json!({
            "cvss": "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H"
        }));
        let score = extract_cvss_score(&db).unwrap();
        assert!((score - 9.8).abs() < 0.05, "score={}", score);
    }

    #[test]
    fn test_extract_cvss_score_missing_returns_none() {
        // 无 score / cvss 字段 → None
        assert!(extract_cvss_score(&None).is_none());
        assert!(extract_cvss_score(&Some(serde_json::json!({ "license": "CC0-1.0" }))).is_none());
    }
}
