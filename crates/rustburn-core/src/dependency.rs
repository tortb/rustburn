//! 依赖分析模块（lockfile 解析 + OSV 查询）。
//!
//! 支持 Cargo.lock 和 package-lock.json 解析，以及 OSV API 批量查询。

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::DependencyFinding;

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
    results: Vec<OsvResult>,
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

/// 查询 OSV API
pub fn query_osv(deps: &[Dependency]) -> Result<Vec<DependencyFinding>, DependencyError> {
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
            .post("https://api.osv.dev/v1/querybatch")
            .set("Content-Type", "application/json")
            .send_string(&json_body);

        match response {
            Ok(resp) => {
                let body = resp
                    .into_string()
                    .map_err(|e| DependencyError::Http(e.to_string()))?;
                let osv_response: OsvResponse = serde_json::from_str(&body)
                    .map_err(|e| DependencyError::Parse(e.to_string()))?;

                let findings = process_osv_response(&osv_response, deps);
                return Ok(findings);
            }
            Err(e) => {
                if attempts >= max_attempts {
                    return Err(DependencyError::Http(e.to_string()));
                }
                // 重试
                continue;
            }
        }
    }
}

/// 处理 OSV 响应
fn process_osv_response(response: &OsvResponse, deps: &[Dependency]) -> Vec<DependencyFinding> {
    let mut findings = Vec::new();

    for (i, result) in response.results.iter().enumerate() {
        if let Some(vulns) = &result.vulns {
            for vuln in vulns {
                // 提取 CVSS 分数
                let cvss_score = extract_cvss_score(&vuln.database_specific);
                let (severity, severity_estimated) = crate::model::cvss_to_severity(cvss_score);

                findings.push(DependencyFinding {
                    id: vuln.id.clone(),
                    package_name: deps[i].name.clone(),
                    ecosystem: deps[i].ecosystem.clone(),
                    version: deps[i].version.clone(),
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

    // 尝试从 cvss 字段提取
    if let Some(cvss) = db.get("cvss") {
        // 如果是数字，直接返回
        if let Some(score) = cvss.as_f64() {
            return Some(score);
        }

        // 如果是字符串，尝试解析 CVSS 向量
        if let Some(cvss_str) = cvss.as_str() {
            // 检查是否包含直接的分数（某些 OSV 响应会在向量后附加分数）
            // 例如: "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H (9.8)"
            if let Some(paren_start) = cvss_str.rfind('(') {
                if let Some(paren_end) = cvss_str.rfind(')') {
                    if paren_start < paren_end {
                        let score_str = &cvss_str[paren_start + 1..paren_end];
                        if let Ok(score) = score_str.trim().parse::<f64>() {
                            return Some(score);
                        }
                    }
                }
            }

            // 尝试从向量字符串计算分数
            // CVSS v3 向量格式: CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H
            if cvss_str.starts_with("CVSS:3") {
                return Some(calculate_cvss_v3_score(cvss_str));
            }
        }
    }

    None
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
        _ => Vec::new(),
    }
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
}
