//! 集成测试：更新检测与安装机制。
//!
//! 覆盖：
//! - 网络超时场景下不 panic、不长时间阻塞；
//! - --offline / RUSTBURN_NO_UPDATE_CHECK 禁用检测；
//! - 24 小时缓存（last_check 时间戳）；
//! - 原子替换：校验失败时原二进制必须保持不变。

mod common;

use std::fs;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::MockServer;
use rustburn_core::update::{
    check_update_silently, extract_checksum, is_newer, latest_release, mark_check, replace_binary,
    sha256_file, should_check_update, update_check_enabled, update_to_latest, UpdateError,
};

/// 网络超时：慢速 server 应在 timeout 内返回错误，而非长时间阻塞。
#[test]
fn latest_release_times_out_without_blocking() {
    let server = MockServer::start(|_method, _path, _body| {
        std::thread::sleep(Duration::from_secs(5));
        (200, "application/json", b"{}".to_vec())
    });

    let start = Instant::now();
    let result = latest_release(&server.addr, Duration::from_millis(300));
    let elapsed = start.elapsed();

    assert!(result.is_err(), "慢 server 应触发超时错误");
    assert!(
        elapsed < Duration::from_secs(2),
        "超时不得阻塞主流程，实际耗时 {:?}",
        elapsed
    );
}

/// 版本比较。
#[test]
fn version_comparison() {
    assert!(is_newer("v0.2.0", "0.1.1"));
    assert!(is_newer("0.1.10", "0.1.9"));
    assert!(!is_newer("v0.1.1", "0.1.1"));
    assert!(!is_newer("v0.1.0", "0.1.1"));
    assert!(!is_newer("not-a-version", "0.1.1"));
    assert!(!is_newer("v0.1.1", "not-a-version"));
}

/// 24 小时缓存：最近检查过则不重复请求；超过 24 小时重新检查。
#[test]
fn update_check_respects_24h_cache() {
    let dir = tempfile::tempdir().unwrap();
    let interval = Duration::from_secs(24 * 3600);

    // 无 last_check → 应检查
    assert!(should_check_update(dir.path(), interval));

    // 刚检查过 → 不应重复检查
    mark_check(dir.path()).unwrap();
    assert!(!should_check_update(dir.path(), interval));

    // 25 小时前检查 → 应重新检查
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 25 * 3600;
    fs::write(dir.path().join("last_check"), ts.to_string()).unwrap();
    assert!(should_check_update(dir.path(), interval));

    // last_check 内容损坏 → 视为应检查（不 panic）
    fs::write(dir.path().join("last_check"), "garbage").unwrap();
    assert!(should_check_update(dir.path(), interval));
}

/// 禁用：--offline 与 RUSTBURN_NO_UPDATE_CHECK 都能关闭检测。
#[test]
fn update_check_can_be_disabled() {
    // --offline
    assert!(!update_check_enabled(true));

    // 环境变量
    std::env::set_var("RUSTBURN_NO_UPDATE_CHECK", "1");
    assert!(!update_check_enabled(false));
    std::env::remove_var("RUSTBURN_NO_UPDATE_CHECK");
    assert!(update_check_enabled(false));
}

/// 静默检查：缓存命中时不发网络请求（server 请求计数为 0）。
#[test]
fn check_update_silently_hits_cache_without_network() {
    let server = MockServer::start(|_m, _p, _b| (200, "application/json", b"{}".to_vec()));
    let dir = tempfile::tempdir().unwrap();

    // 先写入新鲜的时间戳
    mark_check(dir.path()).unwrap();

    let result = check_update_silently(
        dir.path(),
        Duration::from_secs(24 * 3600),
        &server.addr,
        Duration::from_millis(500),
        "0.1.1",
    )
    .unwrap();
    assert!(result.is_none());
    assert_eq!(server.requests(), 0, "缓存命中时不应发起任何网络请求");
}

/// 原子替换：校验失败时原二进制必须保持不变（绝不先删除再写入）。
#[test]
fn update_checksum_mismatch_keeps_original_binary() {
    let server = MockServer::start(|_method, path, _body| {
        if path.ends_with(".tar.gz") {
            (
                200,
                "application/octet-stream",
                b"not-a-real-tarball".to_vec(),
            )
        } else if path.ends_with("SHA256SUMS") {
            (
                200,
                "text/plain",
                b"0000000000000000000000000000000000000000000000000000000000000000  rb-v0.9.9-x86_64-unknown-linux-gnu.tar.gz\n"
                    .to_vec(),
            )
        } else {
            (404, "text/plain", Vec::new())
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("rb");
    fs::write(&exe, "original-binary").unwrap();

    let result = update_to_latest(
        &server.addr,
        "v0.9.9",
        "x86_64-unknown-linux-gnu",
        &exe,
        Duration::from_secs(5),
    );

    assert!(matches!(result, Err(UpdateError::ChecksumMismatch { .. })));
    // 原文件保持不变
    assert_eq!(
        fs::read_to_string(&exe).unwrap(),
        "original-binary",
        "校验失败不得覆盖现有可执行文件"
    );
    // 临时文件已清理
    let leftovers: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".rb-update-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "失败后临时文件应被清理: {:?}",
        leftovers
    );
}

/// 原子替换成功路径：校验通过后原二进制被替换为新内容。
#[test]
fn update_success_replaces_binary_atomically() {
    // 构造真实 tar.gz（内含 rb）并计算 SHA256
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("rb");
    fs::write(&src, "v0.9.9-binary-content").unwrap();

    let tar_path = dir.path().join("pkg.tar.gz");
    {
        let file = fs::File::create(&tar_path).unwrap();
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(enc);
        builder
            .append_file("rb", &mut fs::File::open(&src).unwrap())
            .unwrap();
        let enc = builder.into_inner().unwrap();
        enc.finish().unwrap();
    }
    let tar_bytes = fs::read(&tar_path).unwrap();
    let expected_hash = sha256_file(&tar_path).unwrap();

    let sums_line = format!(
        "{}  rb-v0.9.9-x86_64-unknown-linux-gnu.tar.gz\n",
        expected_hash
    );
    let server = MockServer::start(move |_method, path, _body| {
        if path.ends_with(".tar.gz") {
            (200, "application/octet-stream", tar_bytes.clone())
        } else if path.ends_with("SHA256SUMS") {
            (200, "text/plain", sums_line.clone().into_bytes())
        } else {
            (404, "text/plain", Vec::new())
        }
    });

    let exe = dir.path().join("rb-target");
    fs::write(&exe, "old-binary").unwrap();

    update_to_latest(
        &server.addr,
        "v0.9.9",
        "x86_64-unknown-linux-gnu",
        &exe,
        Duration::from_secs(5),
    )
    .expect("update should succeed");

    assert_eq!(
        fs::read_to_string(&exe).unwrap(),
        "v0.9.9-binary-content",
        "校验通过后应原子替换为新二进制"
    );
}

/// replace_binary：直接验证 rename 语义（目标被新内容覆盖）。
#[test]
fn replace_binary_replaces_target() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("rb");
    let new = dir.path().join(".tmp-new");
    fs::write(&exe, "old").unwrap();
    fs::write(&new, "new").unwrap();

    replace_binary(&exe, &new).unwrap();
    assert_eq!(fs::read_to_string(&exe).unwrap(), "new");
    assert!(!new.exists(), "rename 后源文件不应存在");
}

/// SHA256SUMS 解析：支持 `hash  name` 与 `hash  *name` 两种格式。
#[test]
fn checksum_extraction_formats() {
    let sums = "abc  rb-v1-x86.tar.gz\n123 *rb-v2.zip\n";
    assert_eq!(
        extract_checksum(sums, "rb-v1-x86.tar.gz"),
        Some("abc".to_string())
    );
    assert_eq!(extract_checksum(sums, "rb-v2.zip"), Some("123".to_string()));
    assert_eq!(extract_checksum(sums, "missing"), None);
}
