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

/// 跨数据源去重：同一漏洞同时以 GHSA 与 RUSTSEC 两套编号出现
/// （详情 aliases 互指）时，必须只保留一条，避免重复计数。
#[test]
fn alias_vulns_are_deduplicated() {
    let server = MockServer::start(|method, path, _body| {
        if method == "POST" && path == "/v1/querybatch" {
            (
                200,
                "application/json",
                r#"{"results":[{"vulns":[
                    {"id":"GHSA-j39j-6gw9-jw6h","modified":"2026-02-05T14:43:45Z"},
                    {"id":"RUSTSEC-2026-0008","modified":"2026-02-05T06:56:18Z"}
                ]}]}"#
                    .to_string()
                    .into_bytes(),
            )
        } else if method == "GET" && path == "/v1/vulns/GHSA-j39j-6gw9-jw6h" {
            (
                200,
                "application/json",
                r#"{"id":"GHSA-j39j-6gw9-jw6h","summary":"git2 has potential undefined behavior when dereferencing Buf struct","aliases":["RUSTSEC-2026-0008"],"database_specific":{"severity":"LOW"}}"#
                    .to_string()
                    .into_bytes(),
            )
        } else if method == "GET" && path == "/v1/vulns/RUSTSEC-2026-0008" {
            (
                200,
                "application/json",
                r#"{"id":"RUSTSEC-2026-0008","summary":"Potential undefined behavior when dereferencing Buf struct","aliases":["GHSA-j39j-6gw9-jw6h"]}"#
                    .to_string()
                    .into_bytes(),
            )
        } else {
            (404, "application/json", b"{}".to_vec())
        }
    });

    let deps = vec![Dependency {
        name: "git2".to_string(),
        version: "0.19.0".to_string(),
        ecosystem: "crates.io".to_string(),
    }];

    let findings = query_osv_with_base(&deps, &server.addr).expect("query_osv");
    assert_eq!(
        findings.len(),
        1,
        "GHSA 与 RUSTSEC 为同一漏洞（aliases 互指），应只保留一条"
    );
    assert_eq!(
        findings[0].id, "RUSTSEC-2026-0008",
        "应优先保留 RUSTSEC 编号"
    );
    assert_eq!(
        findings[0].severity,
        Severity::Low,
        "保留 GHSA 的真实严重度"
    );
    assert!(!findings[0].severity_estimated);
}

/// 不同漏洞（无 aliases 关联）不得被去重合并。
#[test]
fn distinct_vulns_are_not_deduplicated() {
    let server = MockServer::start(|method, path, _body| {
        if method == "POST" && path == "/v1/querybatch" {
            (
                200,
                "application/json",
                r#"{"results":[{"vulns":[
                    {"id":"RUSTSEC-2026-0008","modified":"2026-02-05T06:56:18Z"},
                    {"id":"RUSTSEC-2026-0183","modified":"2026-06-17T13:00:04Z"}
                ]}]}"#
                    .to_string()
                    .into_bytes(),
            )
        } else if method == "GET" && path == "/v1/vulns/RUSTSEC-2026-0008" {
            (
                200,
                "application/json",
                r#"{"id":"RUSTSEC-2026-0008","summary":"Buf struct UB","aliases":["GHSA-j39j-6gw9-jw6h"]}"#
                    .to_string()
                    .into_bytes(),
            )
        } else if method == "GET" && path == "/v1/vulns/RUSTSEC-2026-0183" {
            (
                200,
                "application/json",
                r#"{"id":"RUSTSEC-2026-0183","summary":"Remote::list() UB","aliases":[]}"#
                    .to_string()
                    .into_bytes(),
            )
        } else {
            (404, "application/json", b"{}".to_vec())
        }
    });

    let deps = vec![Dependency {
        name: "git2".to_string(),
        version: "0.19.0".to_string(),
        ecosystem: "crates.io".to_string(),
    }];

    let findings = query_osv_with_base(&deps, &server.addr).expect("query_osv");
    assert_eq!(findings.len(), 2, "两个独立漏洞不应被去重合并");
    assert_eq!(findings[0].id, "RUSTSEC-2026-0008");
    assert_eq!(findings[1].id, "RUSTSEC-2026-0183");
}
