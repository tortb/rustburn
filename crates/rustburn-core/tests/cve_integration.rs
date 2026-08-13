//! 回归测试：CVE 数据真实性（本地 mock OSV API，不依赖真实网络）。
//!
//! 覆盖 v0.1.1 修复点：
//! - querybatch 只返回 id/modified，摘要必须通过 /v1/vulns/{id} 二次拉取；
//! - 摘要非空且来自详情接口（真实可查证，非编造占位）；
//! - 有真实 severity 时标记为估算=false，无 CVSS 时保持估算=true。

mod common;

use common::MockServer;
use rustburn_core::dependency::{query_osv_with_base, Dependency};
use rustburn_core::model::Severity;

/// 模拟 OSV：批量查询返回带摘要的完整条目 + 一个无漏洞条目。
#[test]
fn cve_findings_carry_real_summary_and_severity() {
    let server = MockServer::start(|method, path, _body| {
        if method == "POST" && path == "/v1/querybatch" {
            (
                200,
                "application/json",
                r#"{"results":[
                    {"vulns":[{"id":"RUSTSEC-2026-9999","modified":"2026-01-01T00:00:00Z"}]},
                    {"vulns":[]}
                ]}"#
                .to_string()
                .into_bytes(),
            )
        } else if method == "GET" && path == "/v1/vulns/RUSTSEC-2026-9999" {
            (
                200,
                "application/json",
                r#"{"id":"RUSTSEC-2026-9999","summary":"Potential undefined behavior in mock crate","database_specific":{"severity":"HIGH"}}"#
                    .to_string()
                    .into_bytes(),
            )
        } else {
            (404, "application/json", b"{}".to_vec())
        }
    });

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

    let findings = query_osv_with_base(&deps, &server.addr).expect("query_osv");
    assert_eq!(findings.len(), 1, "只有第一个依赖有漏洞");
    let f = &findings[0];
    assert_eq!(f.id, "RUSTSEC-2026-9999");
    assert_eq!(f.package_name, "pkg-a");
    assert_eq!(
        f.summary, "Potential undefined behavior in mock crate",
        "摘要必须来自详情接口，而非空占位"
    );
    assert_eq!(
        f.severity,
        Severity::High,
        "真实 severity 来自 database_specific"
    );
    assert!(!f.severity_estimated, "有真实严重度时不得标记为估算");
}

/// 模拟 OSV：详情接口只有 summary、无任何 CVSS/severity 信息时，
/// 严重度应保持"估算 Medium"（诚实标注，而非编造数值）。
#[test]
fn cve_without_cvss_keeps_estimated_severity() {
    let server = MockServer::start(|method, path, _body| {
        if method == "POST" && path == "/v1/querybatch" {
            (
                200,
                "application/json",
                r#"{"results":[{"vulns":[{"id":"RUSTSEC-2026-0001","modified":"2026-01-01T00:00:00Z"}]}]}"#
                    .to_string()
                    .into_bytes(),
            )
        } else if method == "GET" && path == "/v1/vulns/RUSTSEC-2026-0001" {
            (
                200,
                "application/json",
                r#"{"id":"RUSTSEC-2026-0001","summary":"advisory without cvss","database_specific":{"license":"CC0-1.0"}}"#
                    .to_string()
                    .into_bytes(),
            )
        } else {
            (404, "application/json", b"{}".to_vec())
        }
    });

    let deps = vec![Dependency {
        name: "pkg-x".to_string(),
        version: "0.1.0".to_string(),
        ecosystem: "crates.io".to_string(),
    }];

    let findings = query_osv_with_base(&deps, &server.addr).expect("query_osv");
    assert_eq!(findings.len(), 1);
    let f = &findings[0];
    assert_eq!(f.summary, "advisory without cvss");
    assert_eq!(f.severity, Severity::Medium);
    assert!(f.severity_estimated, "无 CVSS 时应保持估算标记");
}

/// 模拟 OSV：详情接口不可达（返回 500）时，
/// 摘要应保留为空、严重度保持估算，但查询本身不应失败。
#[test]
fn cve_detail_failure_degrades_gracefully() {
    let server = MockServer::start(|method, path, _body| {
        if method == "POST" && path == "/v1/querybatch" {
            (
                200,
                "application/json",
                r#"{"results":[{"vulns":[{"id":"RUSTSEC-2026-0002","modified":"2026-01-01T00:00:00Z"}]}]}"#
                    .to_string()
                    .into_bytes(),
            )
        } else {
            (500, "application/json", b"{}".to_vec())
        }
    });

    let deps = vec![Dependency {
        name: "pkg-y".to_string(),
        version: "0.2.0".to_string(),
        ecosystem: "crates.io".to_string(),
    }];

    let findings = query_osv_with_base(&deps, &server.addr).expect("query_osv");
    assert_eq!(findings.len(), 1);
    let f = &findings[0];
    assert_eq!(f.id, "RUSTSEC-2026-0002");
    assert!(f.summary.is_empty(), "详情失败时摘要为空（不编造）");
    assert!(f.severity_estimated);
}
