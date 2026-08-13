//! 更新检测与安装（rb update / scan 后的后台检查）。
//!
//! 安全原则：
//! - 任何更新动作都必须有用户可见的确认步骤（`rb update` 交互确认）；
//! - 下载到临时文件并校验 SHA256 通过后，才用 `rename` 原子替换；
//! - 校验失败 / 网络失败时明确报错，绝不覆盖现有可执行文件。

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use thiserror::Error;

/// 默认的 GitHub Releases API（最新 release）。
pub const DEFAULT_API_URL: &str = "https://api.github.com/repos/tortb/rustburn/releases/latest";
/// 默认的 release 下载基址。
pub const DEFAULT_DL_BASE: &str = "https://github.com/tortb/rustburn/releases/download";

const USER_AGENT: &str = concat!("rustburn/", env!("CARGO_PKG_VERSION"));

#[derive(Error, Debug)]
pub enum UpdateError {
    #[error("network error: {0}")]
    Network(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },
    #[error("release does not provide checksum for {0}")]
    MissingChecksum(String),
    #[error("unsupported platform: {0} {1}")]
    UnsupportedPlatform(String, String),
    #[error("archive does not contain binary: {0}")]
    MissingBinary(String),
}

/// GitHub Releases latest 响应（仅需要 tag_name 与 body）。
#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseInfo {
    pub tag_name: String,
    #[serde(default)]
    pub body: String,
}

/// 查询 GitHub Releases API，返回最新 release 信息。
pub fn latest_release(api_url: &str, timeout: Duration) -> Result<ReleaseInfo, UpdateError> {
    let agent = ureq::AgentBuilder::new().timeout(timeout).build();
    let resp = agent
        .get(api_url)
        .set("Accept", "application/vnd.github+json")
        .set("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| UpdateError::Network(e.to_string()))?;
    let body = resp
        .into_string()
        .map_err(|e| UpdateError::Network(e.to_string()))?;
    serde_json::from_str(&body).map_err(|e| UpdateError::Parse(e.to_string()))
}

/// 版本号比较：latest > current 返回 true（忽略 `v` 前缀，仅支持 x.y.z）。
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim_start_matches('v');
    let mut it = v.split('.');
    Some((
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
    ))
}

/// 当前平台的 release 资产目标三元组（与 release.yml / build-release.sh 一致）。
pub fn platform_target() -> Option<String> {
    let triple = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        _ => return None,
    };
    Some(triple.to_string())
}

/// 计算文件 SHA256（小写十六进制）。
pub fn sha256_file(path: &Path) -> Result<String, UpdateError> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut f = std::fs::File::open(path)?;
    io::copy(&mut f, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// 从 SHA256SUMS 文本中提取指定资产名的校验和（兼容 `name` 与 `*name` 两种格式）。
pub fn extract_checksum(sums_text: &str, asset_name: &str) -> Option<String> {
    sums_text.lines().find_map(|line| {
        let mut it = line.split_whitespace();
        let hash = it.next()?;
        let name = it.next().unwrap_or("");
        let name = name.trim_start_matches('*');
        if name == asset_name {
            Some(hash.to_string())
        } else {
            None
        }
    })
}

/// 本地缓存目录：`~/.cache/rustburn`（Windows 回退到 %LOCALAPPDATA%/rustburn）。
pub fn cache_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME") {
        if !home.is_empty() {
            return Some(PathBuf::from(home).join(".cache/rustburn"));
        }
    }
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("rustburn"))
}

/// 是否需要进行一次更新检查。
///
/// 距离上次成功检查不足 `interval` 时返回 false（24 小时内不重复检测）。
pub fn should_check_update(cache_dir: &Path, interval: Duration) -> bool {
    let last_check_path = cache_dir.join("last_check");
    if let Ok(content) = std::fs::read_to_string(&last_check_path) {
        if let Ok(ts) = content.trim().parse::<u64>() {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if now.saturating_sub(ts) < interval.as_secs() {
                return false;
            }
        }
    }
    true
}

/// 记录本次检查时间戳（失败静默，不阻塞主流程）。
pub fn mark_check(cache_dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(cache_dir)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    std::fs::write(cache_dir.join("last_check"), now.to_string())
}

/// 更新检测是否可用：`--offline` 或环境变量 `RUSTBURN_NO_UPDATE_CHECK` 均禁用。
pub fn update_check_enabled(offline: bool) -> bool {
    !offline && std::env::var_os("RUSTBURN_NO_UPDATE_CHECK").is_none()
}

/// 一次完整的静默检查（幂等）。
///
/// - 距离上次检查不足 `interval` 直接返回 Ok(None)，不发网络请求；
/// - 网络失败 / 解析失败返回 Err（由调用方静默忽略）；
/// - 返回 `Ok(Some(tag))` 表示发现比 `current` 更新的版本。
pub fn check_update_silently(
    cache_dir: &Path,
    interval: Duration,
    api_url: &str,
    timeout: Duration,
    current: &str,
) -> Result<Option<String>, UpdateError> {
    if !should_check_update(cache_dir, interval) {
        return Ok(None);
    }
    let result = match latest_release(api_url, timeout) {
        Ok(release) => Ok(if is_newer(&release.tag_name, current) {
            Some(release.tag_name)
        } else {
            None
        }),
        Err(e) => Err(e),
    };
    // 无论成功失败都记录时间戳，避免反复重试拖慢后续命令
    let _ = mark_check(cache_dir);
    result
}

/// release notes 摘要（截断到 `max_chars` 字符）。
pub fn notes_summary(body: &str, max_chars: usize) -> String {
    let trimmed = body.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(max_chars).collect();
    out.push_str("...");
    out
}

/// 原子替换：用 `rename` 把新二进制替换为目标可执行文件。
///
/// - 同目录内 rename 是原子的：任何时刻目标要么是旧文件、要么是新文件；
/// - **绝不先删除目标再写入**，失败时原文件保持完整；
/// - Windows 上目标存在时 `rename` 会失败，安全失败（不覆盖），由调用方报告。
pub fn replace_binary(exe_path: &Path, new_binary: &Path) -> Result<(), UpdateError> {
    std::fs::rename(new_binary, exe_path)?;
    Ok(())
}

/// 完整更新流程：下载对应平台资产 → SHA256 校验 → 解压 → 原子替换。
///
/// 任何一步失败都会清理临时文件并返回错误，**不会触碰现有可执行文件**。
pub fn update_to_latest(
    dl_base: &str,
    version: &str,
    target: &str,
    exe_path: &Path,
    timeout: Duration,
) -> Result<(), UpdateError> {
    let ext = if cfg!(target_os = "windows") {
        "zip"
    } else {
        "tar.gz"
    };
    let asset = format!("rb-{}-{}.{}", version, target, ext);

    let work_dir = exe_path.parent().ok_or_else(|| {
        UpdateError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            "executable has no parent directory",
        ))
    })?;
    let pid = std::process::id();
    let tmp_archive = work_dir.join(format!(".rb-update-{}.{}", pid, ext));
    let tmp_dir = work_dir.join(format!(".rb-update-{}-extract", pid));

    let cleanup = || {
        let _ = std::fs::remove_file(&tmp_archive);
        let _ = std::fs::remove_dir_all(&tmp_dir);
    };

    let agent = ureq::AgentBuilder::new().timeout(timeout).build();

    // 1. 下载资产到目标同目录临时文件（保证后续 rename 同文件系统）
    download_to(
        &agent,
        &format!("{}/{}/{}", dl_base, version, asset),
        &tmp_archive,
    )?;

    // 2. 下载 SHA256SUMS 并校验（失败绝不继续）
    let sums_text = match download_to_string(&agent, &format!("{}/{}/SHA256SUMS", dl_base, version))
    {
        Ok(text) => text,
        Err(e) => {
            cleanup();
            return Err(e);
        }
    };

    let expected = extract_checksum(&sums_text, &asset).ok_or_else(|| {
        cleanup();
        UpdateError::MissingChecksum(asset.clone())
    })?;
    let actual = sha256_file(&tmp_archive)?;
    if !expected.eq_ignore_ascii_case(&actual) {
        cleanup();
        return Err(UpdateError::ChecksumMismatch { expected, actual });
    }

    // 3. 解压出二进制
    let extract_result = if cfg!(target_os = "windows") {
        extract_binary_from_zip(&tmp_archive, &tmp_dir)
    } else {
        extract_binary_from_tar_gz(&tmp_archive, &tmp_dir)
    };
    let new_binary = match extract_result {
        Ok(path) => path,
        Err(e) => {
            cleanup();
            return Err(e);
        }
    };

    // 4. 原子替换
    let result = replace_binary(exe_path, &new_binary);
    cleanup();
    result
}

/// 下载文件到 `dest`。
fn download_to(agent: &ureq::Agent, url: &str, dest: &Path) -> Result<(), UpdateError> {
    let resp = agent
        .get(url)
        .set("Accept", "application/octet-stream")
        .set("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| UpdateError::Network(e.to_string()))?;
    let mut reader = resp.into_reader();
    let mut out = std::fs::File::create(dest)?;
    io::copy(&mut reader, &mut out)?;
    Ok(())
}

/// 下载文本内容。
fn download_to_string(agent: &ureq::Agent, url: &str) -> Result<String, UpdateError> {
    let resp = agent
        .get(url)
        .set("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| UpdateError::Network(e.to_string()))?;
    resp.into_string()
        .map_err(|e| UpdateError::Network(e.to_string()))
}

/// 从 tar.gz 中解压出 `rb` 文件。
fn extract_binary_from_tar_gz(archive: &Path, dest_dir: &Path) -> Result<PathBuf, UpdateError> {
    std::fs::create_dir_all(dest_dir)?;
    let file = std::fs::File::open(archive)?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut ar = tar::Archive::new(gz);
    for entry in ar.entries()? {
        let mut entry = entry?;
        let is_rb = entry
            .path()
            .ok()
            .and_then(|p| p.file_name().map(|n| n == "rb"))
            .unwrap_or(false);
        if is_rb {
            let dest = dest_dir.join("rb");
            let mut out = std::fs::File::create(&dest)?;
            io::copy(&mut entry, &mut out)?;
            return Ok(dest);
        }
    }
    Err(UpdateError::MissingBinary("rb".to_string()))
}

/// 从 zip 中解压出 `rb.exe` 文件。
fn extract_binary_from_zip(archive: &Path, dest_dir: &Path) -> Result<PathBuf, UpdateError> {
    std::fs::create_dir_all(dest_dir)?;
    let file = std::fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| UpdateError::Parse(e.to_string()))?;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| UpdateError::Parse(e.to_string()))?;
        let is_exe = Path::new(entry.name())
            .file_name()
            .map(|n| n == "rb.exe")
            .unwrap_or(false);
        if is_exe {
            let dest = dest_dir.join("rb.exe");
            let mut out = std::fs::File::create(&dest)?;
            io::copy(&mut entry, &mut out)?;
            return Ok(dest);
        }
    }
    Err(UpdateError::MissingBinary("rb.exe".to_string()))
}
