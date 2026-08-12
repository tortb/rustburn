use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;

use chrono::Utc;
use git2::{DiffFindOptions, Repository, Revwalk};
use regex::Regex;

use crate::model::HistoryRewriteState;

/// 文件级 Git 历史指标。
#[derive(Debug, Clone)]
pub struct FileGitMetrics {
    pub path: String,
    pub commit_count: u32,
    pub distinct_authors: u32,
    pub last_modified_days_ago: u32,
    pub incident_commit_count: u32,
}

/// 获取 incident commit 检测用的正则（懒初始化）。
fn incident_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\b(fix|bug|revert|hotfix|patch|error|crash)\b")
            .expect("incident commit regex should be valid")
    })
}

/// 移除作者 email 的前后空格，并转为小写。空 email 统一为 "unknown-author"。
fn normalize_author_email(email: Option<&str>) -> String {
    let email = email.unwrap_or("").trim();
    if email.is_empty() {
        "unknown-author".to_string()
    } else {
        email.to_lowercase()
    }
}

/// 检查 commit message 是否包含 incident 关键词。
fn is_incident_commit(msg: &str) -> bool {
    incident_regex().is_match(&msg.to_lowercase())
}

/// 构建路径重命名映射表。
///
/// 通过遍历 commit 历史，追踪每次 rename，构建从旧路径到最终 HEAD 路径的映射链。
/// 支持连续重命名：a → b → c → d。
fn build_rename_map(repo: &Repository, max_commits: u32) -> HashMap<String, String> {
    let mut revwalk = match head_revwalk(repo) {
        Ok(rw) => rw,
        Err(_) => return HashMap::new(),
    };

    let mut rename_map: HashMap<String, String> = HashMap::new();
    let mut count: u32 = 0;

    while let Some(oid) = revwalk.next().transpose().ok().flatten() {
        if count >= max_commits {
            break;
        }
        count += 1;

        let commit = match repo.find_commit(oid) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let tree = match commit.tree() {
            Ok(t) => t,
            Err(_) => continue,
        };

        let parent_tree = match commit.parent(0) {
            Ok(p) => p.tree().ok(),
            Err(_) => None,
        };

        let mut opts = DiffFindOptions::new();
        opts.renames(true);
        let mut diff = match repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None) {
            Ok(d) => d,
            Err(_) => continue,
        };

        if diff.find_similar(Some(&mut opts)).is_err() {
            continue;
        }

        for delta in diff.deltas() {
            if delta.status() == git2::Delta::Renamed {
                let old_path = delta
                    .old_file()
                    .path()
                    .map(|p| p.to_string_lossy().into_owned());
                let new_path = delta
                    .new_file()
                    .path()
                    .map(|p| p.to_string_lossy().into_owned());

                if let (Some(old), Some(new)) = (old_path, new_path) {
                    // 如果 new 已经重命名到更新路径，进行链式跟踪
                    let final_path = rename_map.get(&new).cloned().unwrap_or(new);
                    rename_map.insert(old.clone(), final_path.clone());

                    // 中间路径也指向最终路径
                    let mut current = old;
                    while let Some(next) = rename_map.get(&current) {
                        if *next == final_path {
                            break;
                        }
                        current = next.clone();
                    }
                }
            }
        }
    }

    rename_map
}

/// 创建从 HEAD 开始的 revwalk。
fn head_revwalk(repo: &Repository) -> Result<Revwalk<'_>, git2::Error> {
    let head = repo.head()?;
    let head_commit = head.peel_to_commit()?;
    let mut revwalk = repo.revwalk()?;
    revwalk.set_sorting(git2::Sort::TIME | git2::Sort::TOPOLOGICAL)?;
    revwalk.push(head_commit.id())?;
    Ok(revwalk)
}

/// 检测历史重写状态（启发式）。
///
/// 检查 .git/logs/HEAD 文件是否存在且可读。
/// 如果无法可靠判断，返回 Unknown。
/// 如果检测到异常（如 reflog 异常短、存在 force-push 迹象），返回 Detected。
pub fn detect_history_rewrite(repo_path: &Path) -> HistoryRewriteState {
    let log_path = repo_path.join(".git").join("logs").join("HEAD");
    let content = match std::fs::read_to_string(&log_path) {
        Ok(c) => c,
        Err(_) => return HistoryRewriteState::Unknown,
    };

    if content.trim().is_empty() {
        return HistoryRewriteState::Unknown;
    }

    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return HistoryRewriteState::Unknown;
    }

    // 启发式检测：检查 reflog 中是否存在 force-push 或 rebase 迹象
    // reflog 格式: <old-sha> <new-sha> <author> <timestamp> <action>: <message>
    let mut suspicious_count = 0;

    for line in &lines {
        let line_lower = line.to_lowercase();
        // 检测常见的历史重写动作
        if line_lower.contains("reset:")
            || line_lower.contains("rebase")
            || line_lower.contains("amend")
            || line_lower.contains("force")
        {
            suspicious_count += 1;
        }
    }

    // 如果检测到多个可疑动作，标记为 Detected
    if suspicious_count > 0 {
        HistoryRewriteState::Detected
    } else {
        // v1：无法可靠证明 force-push，返回 Unknown
        HistoryRewriteState::Unknown
    }
}

/// 检查仓库是否为空（无 commit）。
pub fn is_empty_repo(repo: &Repository) -> bool {
    repo.head().is_err()
}

/// 核心函数：分析 Git 仓库历史，返回每个 HEAD 文件的 Git 历史指标。
///
/// # 参数
/// - `repo_path`: 仓库路径
/// - `max_commits`: 最大处理 commit 数量，默认 5000
///
/// # 返回
/// - `HashMap<String, FileGitMetrics>`: 文件路径 -> Git 历史指标
/// - `bool`: 历史是否被截断
/// - `HistoryRewriteState`: 历史重写检测状态
pub fn analyze_git_history(
    repo_path: &Path,
    max_commits: u32,
) -> Result<(HashMap<String, FileGitMetrics>, bool, HistoryRewriteState), anyhow::Error> {
    let repo =
        Repository::open(repo_path).map_err(|e| anyhow::anyhow!("repository_not_found: {}", e))?;

    let history_rewrite = detect_history_rewrite(repo_path);

    // 空仓库处理
    if is_empty_repo(&repo) {
        return Ok((HashMap::new(), false, history_rewrite));
    }

    // 构建 rename map
    let rename_map = build_rename_map(&repo, max_commits);

    let mut revwalk = head_revwalk(&repo)?;

    // 按文件跟踪数据
    let mut file_commits: HashMap<String, u32> = HashMap::new();
    let mut file_authors: HashMap<String, HashSet<String>> = HashMap::new();
    let mut file_incidents: HashMap<String, u32> = HashMap::new();
    let mut file_last_modified: HashMap<String, i64> = HashMap::new(); // unix timestamp
    let mut processed_commits: u32 = 0;
    let mut history_truncated = false;

    let now = Utc::now();

    while let Some(oid) = revwalk.next().transpose().ok().flatten() {
        if processed_commits >= max_commits {
            history_truncated = true;
            break;
        }
        processed_commits += 1;

        let commit = match repo.find_commit(oid) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let commit_time = commit.time().seconds();
        let author = normalize_author_email(commit.author().email());
        let msg = commit.message().unwrap_or("");
        let is_incident = is_incident_commit(msg);

        let tree = match commit.tree() {
            Ok(t) => t,
            Err(_) => continue,
        };

        let parent_tree = match commit.parent(0) {
            Ok(p) => p.tree().ok(),
            Err(_) => None,
        };

        let mut diff_opts = DiffFindOptions::new();
        diff_opts.renames(true);
        let mut diff = match repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None) {
            Ok(d) => d,
            Err(_) => continue,
        };

        if diff.find_similar(Some(&mut diff_opts)).is_err() {
            continue;
        }

        // 收集此 commit 中所有被修改的唯一文件路径（去重）
        let mut modified_files: HashSet<String> = HashSet::new();

        for delta in diff.deltas() {
            // 获取新文件的路径
            let file_path = delta
                .new_file()
                .path()
                .map(|p| p.to_string_lossy().into_owned());

            if let Some(ref path) = file_path {
                // 检查是否为源码文件（非二进制）
                if delta.new_file().is_binary() {
                    continue;
                }

                // 处理路径重命名
                let canonical_path = rename_map
                    .get(path)
                    .cloned()
                    .unwrap_or_else(|| path.clone());

                modified_files.insert(canonical_path);
            }
        }

        // 空 commit 跳过
        if modified_files.is_empty() {
            continue;
        }

        // 对每个唯一文件路径，只增加一次 commit_count 和 incident_commit_count
        for canonical_path in modified_files {
            // commit count：同一 commit 中无论修改多少次同一文件，只计 1 次
            *file_commits.entry(canonical_path.clone()).or_insert(0) += 1;

            // authors
            file_authors
                .entry(canonical_path.clone())
                .or_default()
                .insert(author.clone());

            // incident commits（同一 commit 同一文件只计 1 次）
            if is_incident {
                *file_incidents.entry(canonical_path.clone()).or_insert(0) += 1;
            }

            // last modified
            let existing = file_last_modified.get(&canonical_path).copied();
            if existing.is_none() || existing.unwrap() < commit_time {
                file_last_modified.insert(canonical_path.clone(), commit_time);
            }
        }
    }

    // 构建结果
    let mut results = HashMap::new();

    // 从 HEAD tree 获取当前文件列表
    if let Ok(head) = repo.head() {
        if let Ok(head_commit) = head.peel_to_commit() {
            if let Ok(tree) = head_commit.tree() {
                // 遍历 HEAD tree（使用递归遍历）
                collect_tree_entries(&repo, &tree, "", &mut |path| {
                    let canonical = rename_map.get(&path).cloned().unwrap_or(path.clone());

                    let metrics = FileGitMetrics {
                        path: canonical.clone(),
                        commit_count: file_commits.get(&canonical).copied().unwrap_or(0),
                        distinct_authors: file_authors
                            .get(&canonical)
                            .map(|a| a.len() as u32)
                            .unwrap_or(0),
                        last_modified_days_ago: file_last_modified
                            .get(&canonical)
                            .map(|ts| {
                                let days = (now.timestamp() - ts) / 86400;
                                days.max(0) as u32
                            })
                            .unwrap_or(0),
                        incident_commit_count: file_incidents.get(&canonical).copied().unwrap_or(0),
                    };
                    results.insert(canonical, metrics);
                });
            }
        }
    }

    Ok((results, history_truncated, history_rewrite))
}

/// 递归遍历 git tree 收集文件路径。
fn collect_tree_entries(
    repo: &Repository,
    tree: &git2::Tree,
    prefix: &str,
    f: &mut dyn FnMut(String),
) {
    for entry in tree.iter() {
        let name = entry.name().unwrap_or("");
        let path = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", prefix, name)
        };

        match entry.kind() {
            Some(git2::ObjectType::Blob) => {
                f(path);
            }
            Some(git2::ObjectType::Tree) => {
                if let Ok(obj) = entry.to_object(repo) {
                    if let Ok(subtree) = obj.peel_to_tree() {
                        collect_tree_entries(repo, &subtree, &path, f);
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// 创建临时 Git 仓库，返回 TempDir 和 Repository。
    fn create_temp_repo() -> (TempDir, Repository) {
        let dir = TempDir::new().expect("create temp dir");
        let repo = Repository::init(dir.path()).expect("init repo");
        (dir, repo)
    }

    /// 在仓库中创建文件并提交。
    fn commit_file(
        repo: &Repository,
        path: &str,
        content: &str,
        message: &str,
        author_name: &str,
        author_email: &str,
    ) {
        let dir = repo.workdir().expect("workdir");
        let file_path = dir.join(path);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).ok();
        }
        fs::write(&file_path, content).expect("write file");

        let mut index = repo.index().expect("get index");
        index.add_path(Path::new(path)).expect("add file");
        index.write().expect("write index");

        let tree_id = index.write_tree().expect("write tree");
        let tree = repo.find_tree(tree_id).expect("find tree");

        let signature = git2::Signature::new(author_name, author_email, &git2::Time::new(0, 0))
            .expect("create signature");

        let head = repo.head().ok();
        let parents: Vec<git2::Commit> = head
            .iter()
            .filter_map(|h| h.peel_to_commit().ok())
            .collect();
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();

        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parent_refs,
        )
        .expect("commit");
    }

    #[test]
    fn test_empty_repo() {
        let (dir, repo) = create_temp_repo();
        // 没有 init 会失败，所以已经 init 了
        drop(repo);
        // 重新打开检查
        let repo = Repository::open(dir.path()).expect("open repo");
        assert!(is_empty_repo(&repo));
    }

    #[test]
    fn test_single_commit() {
        let (dir, repo) = create_temp_repo();
        commit_file(
            &repo,
            "main.rs",
            "fn main() {}",
            "initial",
            "Alice",
            "alice@example.com",
        );

        let (metrics, truncated, _) = analyze_git_history(dir.path(), 5000).expect("analyze");
        assert!(!truncated);
        assert!(metrics.contains_key("main.rs"));
        let m = &metrics["main.rs"];
        assert_eq!(m.commit_count, 1);
        assert_eq!(m.distinct_authors, 1);
    }

    #[test]
    fn test_multiple_commits() {
        let (dir, repo) = create_temp_repo();
        commit_file(&repo, "lib.rs", "// v1", "first", "A", "a@test.com");
        commit_file(&repo, "lib.rs", "// v2", "second", "A", "a@test.com");
        commit_file(&repo, "lib.rs", "// v3", "third", "A", "a@test.com");

        let (metrics, _, _) = analyze_git_history(dir.path(), 5000).expect("analyze");
        let m = &metrics["lib.rs"];
        assert_eq!(m.commit_count, 3);
    }

    #[test]
    fn test_multiple_authors() {
        let (dir, repo) = create_temp_repo();
        commit_file(&repo, "main.rs", "v1", "c1", "Alice", "alice@test.com");
        commit_file(&repo, "main.rs", "v2", "c2", "Bob", "bob@test.com");

        let (metrics, _, _) = analyze_git_history(dir.path(), 5000).expect("analyze");
        let m = &metrics["main.rs"];
        assert_eq!(m.distinct_authors, 2);
    }

    #[test]
    fn test_incident_fix() {
        let (dir, repo) = create_temp_repo();
        commit_file(&repo, "bug.rs", "buggy", "initial", "A", "a@t.com");
        commit_file(&repo, "bug.rs", "fixed", "fix login bug", "A", "a@t.com");

        let (metrics, _, _) = analyze_git_history(dir.path(), 5000).expect("analyze");
        let m = &metrics["bug.rs"];
        assert_eq!(m.incident_commit_count, 1);
    }

    #[test]
    fn test_incident_bug() {
        let (dir, repo) = create_temp_repo();
        commit_file(&repo, "f.rs", "v1", "initial", "A", "a@t.com");
        commit_file(&repo, "f.rs", "v2", "fix a bug", "A", "a@t.com");

        let (metrics, _, _) = analyze_git_history(dir.path(), 5000).expect("analyze");
        assert_eq!(metrics["f.rs"].incident_commit_count, 1);
    }

    #[test]
    fn test_incident_revert() {
        let (dir, repo) = create_temp_repo();
        commit_file(&repo, "f.rs", "v1", "initial", "A", "a@t.com");
        commit_file(&repo, "f.rs", "v2", "revert change", "A", "a@t.com");

        let (metrics, _, _) = analyze_git_history(dir.path(), 5000).expect("analyze");
        assert_eq!(metrics["f.rs"].incident_commit_count, 1);
    }

    #[test]
    fn test_incident_hotfix() {
        let (dir, repo) = create_temp_repo();
        commit_file(&repo, "f.rs", "v1", "initial", "A", "a@t.com");
        commit_file(&repo, "f.rs", "v2", "hotfix applied", "A", "a@t.com");

        let (metrics, _, _) = analyze_git_history(dir.path(), 5000).expect("analyze");
        assert_eq!(metrics["f.rs"].incident_commit_count, 1);
    }

    #[test]
    fn test_incident_patch() {
        let (dir, repo) = create_temp_repo();
        commit_file(&repo, "f.rs", "v1", "init", "A", "a@t.com");
        commit_file(&repo, "f.rs", "v2", "security patch", "A", "a@t.com");

        let (metrics, _, _) = analyze_git_history(dir.path(), 5000).expect("analyze");
        assert_eq!(metrics["f.rs"].incident_commit_count, 1);
    }

    #[test]
    fn test_incident_error() {
        let (dir, repo) = create_temp_repo();
        commit_file(&repo, "f.rs", "v1", "init", "A", "a@t.com");
        commit_file(&repo, "f.rs", "v2", "fix error", "A", "a@t.com");

        let (metrics, _, _) = analyze_git_history(dir.path(), 5000).expect("analyze");
        assert_eq!(metrics["f.rs"].incident_commit_count, 1);
    }

    #[test]
    fn test_incident_crash() {
        let (dir, repo) = create_temp_repo();
        commit_file(&repo, "f.rs", "v1", "init", "A", "a@t.com");
        commit_file(&repo, "f.rs", "v2", "fix crash", "A", "a@t.com");

        let (metrics, _, _) = analyze_git_history(dir.path(), 5000).expect("analyze");
        assert_eq!(metrics["f.rs"].incident_commit_count, 1);
    }

    #[test]
    fn test_fixture_not_matched() {
        let (dir, repo) = create_temp_repo();
        commit_file(&repo, "f.rs", "v1", "initial", "A", "a@t.com");
        commit_file(&repo, "f.rs", "v2", "add fixture", "A", "a@t.com");

        let (metrics, _, _) = analyze_git_history(dir.path(), 5000).expect("analyze");
        // "fixture" 不应匹配（不是完整单词 fix）
        assert_eq!(metrics["f.rs"].incident_commit_count, 0);
    }

    #[test]
    fn test_prefix_not_matched() {
        let (dir, repo) = create_temp_repo();
        commit_file(&repo, "f.rs", "v1", "initial", "A", "a@t.com");
        commit_file(&repo, "f.rs", "v2", "prefix update", "A", "a@t.com");

        let (metrics, _, _) = analyze_git_history(dir.path(), 5000).expect("analyze");
        assert_eq!(metrics["f.rs"].incident_commit_count, 0);
    }

    #[test]
    fn test_rename_detection() {
        let (dir, repo) = create_temp_repo();
        // 需要有内容才能检测 rename
        commit_file(&repo, "old.rs", "fn main() {\n    // enough content for similarity\n    let x = 1;\n    let y = 2;\n    println!(\"{}\", x + y);\n}\n", "add old.rs", "A", "a@t.com");

        // 通过 git mv 来重命名
        let workdir = repo.workdir().expect("workdir");
        fs::rename(workdir.join("old.rs"), workdir.join("new.rs")).expect("rename file");

        let mut index = repo.index().expect("get index");
        index.remove(Path::new("old.rs"), 0).ok();
        index.add_path(Path::new("new.rs")).expect("add new.rs");
        index.write().expect("write index");

        let tree_id = index.write_tree().expect("write tree");
        let tree = repo.find_tree(tree_id).expect("find tree");
        let sig = git2::Signature::new("A", "a@t.com", &git2::Time::new(1, 0)).expect("sig");
        let head = repo.head().ok();
        let parents: Vec<git2::Commit> = head
            .iter()
            .filter_map(|h| h.peel_to_commit().ok())
            .collect();
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            "rename old to new",
            &tree,
            &parent_refs,
        )
        .expect("commit");

        // 再修改 new.rs 增加一次 commit
        commit_file(
            &repo,
            "new.rs",
            "fn main() {\n    println!(\"hello\");\n}\n",
            "update new",
            "B",
            "b@t.com",
        );

        let (metrics, _, _) = analyze_git_history(dir.path(), 5000).expect("analyze");

        // new.rs 应该继承了 old.rs 的 commit 历史
        assert!(metrics.contains_key("new.rs"), "new.rs should exist");
        let m = &metrics["new.rs"];
        // commit count 应该 >= 3（old.rs 1个 + rename 1个 + update 1个）
        assert!(
            m.commit_count >= 2,
            "rename should preserve commit history: got {}",
            m.commit_count
        );
    }

    #[test]
    fn test_max_commits_truncation() {
        let (dir, repo) = create_temp_repo();
        for i in 0..10 {
            commit_file(
                &repo,
                "f.rs",
                &format!("// v{}", i),
                &format!("commit {}", i),
                "A",
                "a@t.com",
            );
        }

        let (_, truncated, _) = analyze_git_history(dir.path(), 3).expect("analyze");
        assert!(truncated, "history should be truncated with max_commits=3");
    }

    #[test]
    fn test_empty_commit_handling() {
        let (dir, _repo) = create_temp_repo();
        // 空仓库应该正常返回
        let (metrics, truncated, _) = analyze_git_history(dir.path(), 5000).expect("analyze");
        assert!(!truncated);
        assert!(metrics.is_empty());
    }

    #[test]
    fn test_multiple_files_same_commit() {
        let (dir, repo) = create_temp_repo();
        // 在同一 commit 中修改多个文件
        let workdir = repo.workdir().expect("workdir");
        fs::write(workdir.join("a.rs"), "// a").expect("write");
        fs::write(workdir.join("b.rs"), "// b").expect("write");

        let mut index = repo.index().expect("index");
        index.add_path(Path::new("a.rs")).expect("add a");
        index.add_path(Path::new("b.rs")).expect("add b");
        index.write().expect("write idx");

        let tree_id = index.write_tree().expect("tree");
        let tree = repo.find_tree(tree_id).expect("find tree");
        let sig = git2::Signature::new("A", "a@t.com", &git2::Time::new(0, 0)).expect("sig");
        repo.commit(Some("HEAD"), &sig, &sig, "add files", &tree, &[])
            .expect("commit");

        let (metrics, _, _) = analyze_git_history(dir.path(), 5000).expect("analyze");
        assert_eq!(metrics.len(), 2);
        assert_eq!(metrics["a.rs"].commit_count, 1);
        assert_eq!(metrics["b.rs"].commit_count, 1);
    }

    #[test]
    fn test_same_file_multiple_changes_in_one_commit() {
        let (dir, repo) = create_temp_repo();
        commit_file(&repo, "f.rs", "v1", "init", "A", "a@t.com");
        commit_file(&repo, "f.rs", "v2", "update", "A", "a@t.com");

        let (metrics, _, _) = analyze_git_history(dir.path(), 5000).expect("analyze");
        // 每个文件的每个 commit 中的多次修改只计 1
        assert_eq!(metrics["f.rs"].commit_count, 2);
    }
}
