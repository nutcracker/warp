use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use command::blocking::Command;
use instant::Instant;
use tempfile::TempDir;

use super::{
    CommitCache, CommitCandidate, CommitInfo, CommitLookup, NOT_A_COMMIT_TTL, commit_candidate,
    commit_web_url, lookup_commit, lookup_commit_cached,
};

const HASH: &str = "ab5d5f6cd0123456789abcdef0123456789abcde";

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test Author")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "Test Author")
        .env("GIT_COMMITTER_EMAIL", "author@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .expect("git should run");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn repo_with_commit(message: &str) -> (TempDir, String) {
    let dir = TempDir::new().unwrap();
    git(dir.path(), &["init", "-q"]);
    git(
        dir.path(),
        &[
            "commit",
            "-q",
            "--allow-empty",
            "--no-verify",
            "-m",
            message,
        ],
    );
    let head = git(dir.path(), &["rev-parse", "HEAD"]);
    (dir, head)
}

fn info(full_hash: &str) -> CommitInfo {
    CommitInfo {
        full_hash: full_hash.to_owned(),
        subject: "Subject".to_owned(),
        body: String::new(),
        author: "Author".to_owned(),
        timestamp: 0,
        web_url: None,
    }
}

#[test]
fn commit_candidate_accepts_hashes_and_trims_punctuation() {
    let cases = [
        ("ab5d5f6cd", Some(("ab5d5f6cd", 0, 0))),
        ("ab5d5f6", Some(("ab5d5f6", 0, 0))),
        ("ab5d5f6cd,", Some(("ab5d5f6cd", 0, 1))),
        ("ab5d5f6cd.", Some(("ab5d5f6cd", 0, 1))),
        ("(ab5d5f6cd)", Some(("ab5d5f6cd", 1, 1))),
        ("`ab5d5f6cd`", Some(("ab5d5f6cd", 1, 1))),
        ("**ab5d5f6cd**:", Some(("ab5d5f6cd", 2, 3))),
        (HASH, Some((HASH, 0, 0))),
        ("2026100", Some(("2026100", 0, 0))),
        ("ab5d5f", None),
        ("ab5d5f6cdZ", None),
        ("AB5D5F6CD", None),
        ("ab5d5f6cd0123456789abcdef0123456789abcdef0", None),
        ("ab5d-5f6cd", None),
        ("", None),
        ("...", None),
    ];
    assert!(cases.len() >= 8);
    for (fragment, expected) in cases {
        let expected = expected.map(|(hash, leading_trim, trailing_trim)| CommitCandidate {
            hash,
            leading_trim,
            trailing_trim,
        });
        assert_eq!(
            commit_candidate(fragment),
            expected,
            "fragment {fragment:?}"
        );
    }
}

#[test]
fn commit_web_url_maps_known_hosts() {
    let cases = [
        (
            "git@github.com:nutcracker/warp.git",
            Some(format!("https://github.com/nutcracker/warp/commit/{HASH}")),
        ),
        (
            "https://github.com/warpdotdev/warp.git",
            Some(format!("https://github.com/warpdotdev/warp/commit/{HASH}")),
        ),
        (
            "https://github.com/warpdotdev/warp",
            Some(format!("https://github.com/warpdotdev/warp/commit/{HASH}")),
        ),
        (
            "ssh://git@github.com/warpdotdev/warp.git",
            Some(format!("https://github.com/warpdotdev/warp/commit/{HASH}")),
        ),
        (
            "git@gitlab.com:group/subgroup/project.git",
            Some(format!(
                "https://gitlab.com/group/subgroup/project/-/commit/{HASH}"
            )),
        ),
        (
            "https://oauth2:token@gitlab.example.org/team/app.git",
            Some(format!(
                "https://gitlab.example.org/team/app/-/commit/{HASH}"
            )),
        ),
        (
            "https://gitlab.acme.io/team/app.git",
            Some(format!("https://gitlab.acme.io/team/app/-/commit/{HASH}")),
        ),
        (
            "git@bitbucket.org:team/repo.git",
            Some(format!("https://bitbucket.org/team/repo/commits/{HASH}")),
        ),
        ("git@git.example.com:team/repo.git", None),
        ("/srv/git/repo.git", None),
        ("git@github.com:", None),
    ];
    for (origin, expected) in cases {
        assert_eq!(commit_web_url(origin, HASH), expected, "origin {origin:?}");
    }
}

#[test]
fn lookup_finds_real_commit_with_details() {
    let (dir, head) = repo_with_commit("Add the thing\n\nWhy it was added.");
    git(
        dir.path(),
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:nutcracker/warp.git",
        ],
    );

    let CommitLookup::Commit(info) = lookup_commit(dir.path(), &head[..9]) else {
        panic!("expected a commit");
    };
    assert_eq!(info.full_hash, head);
    assert_eq!(info.subject, "Add the thing");
    assert_eq!(info.body, "Why it was added.");
    assert_eq!(info.author, "Test Author");
    assert_eq!(info.timestamp, 1_700_000_000);
    assert_eq!(
        info.web_url,
        Some(format!("https://github.com/nutcracker/warp/commit/{head}"))
    );
}

#[test]
fn lookup_without_origin_has_no_web_url() {
    let (dir, head) = repo_with_commit("Only commit");
    let CommitLookup::Commit(info) = lookup_commit(dir.path(), &head[..7]) else {
        panic!("expected a commit");
    };
    assert_eq!(info.web_url, None);
}

#[test]
fn lookup_of_hex_word_that_is_not_a_commit_is_not_a_commit() {
    let (dir, head) = repo_with_commit("Only commit");
    // Positive control in the same repo, so a NotACommit below can't come from a broken repo.
    assert!(matches!(
        lookup_commit(dir.path(), &head[..7]),
        CommitLookup::Commit(_)
    ));
    // Make sure the word can't accidentally prefix the only commit.
    let word = if head.starts_with("deadbeef") {
        "facade1"
    } else {
        "deadbeef"
    };
    assert_eq!(lookup_commit(dir.path(), word), CommitLookup::NotACommit);
}

#[test]
fn lookup_of_a_tree_hash_is_not_a_commit() {
    let (dir, _) = repo_with_commit("Only commit");
    let tree = git(dir.path(), &["rev-parse", "HEAD^{tree}"]);
    assert_eq!(lookup_commit(dir.path(), &tree), CommitLookup::NotACommit);
}

#[test]
fn lookup_outside_a_repo_is_unknown() {
    let dir = TempDir::new().unwrap();
    assert!(matches!(
        lookup_commit(dir.path(), HASH),
        CommitLookup::Unknown(_)
    ));
}

#[test]
fn lookup_of_ambiguous_prefix_is_unknown() {
    let (dir, _) = repo_with_commit("Root");
    let tree = git(dir.path(), &["rev-parse", "HEAD^{tree}"]);

    // Create commits until two share a four-character prefix.
    let mut by_prefix = HashMap::new();
    let mut ambiguous = None;
    for i in 0..5000 {
        let hash = git(dir.path(), &["commit-tree", &tree, "-m", &i.to_string()]);
        if by_prefix
            .insert(hash[..4].to_owned(), hash.clone())
            .is_some()
        {
            ambiguous = Some(hash[..4].to_owned());
            break;
        }
    }
    let prefix = ambiguous.expect("two commits should share a 4-character prefix");

    let lookup = lookup_commit(dir.path(), &prefix);
    assert!(
        matches!(&lookup, CommitLookup::Unknown(reason) if reason.contains("ambiguous")),
        "got {lookup:?}"
    );
}

#[test]
fn cache_never_stores_unknown() {
    let mut cache = CommitCache::default();
    let dir = Path::new("/repo");
    let now = Instant::now();
    cache.record(
        dir,
        "ab5d5f6",
        &CommitLookup::Unknown("timed out".into()),
        now,
    );
    assert_eq!(cache.get(dir, "ab5d5f6", now), None);
}

#[test]
fn cache_keeps_commits_and_expires_not_a_commit() {
    let mut cache = CommitCache::default();
    let dir = Path::new("/repo");
    let start = Instant::now();
    let commit = CommitLookup::Commit(info(HASH));
    cache.record(dir, "ab5d5f6", &commit, start);
    cache.record(dir, "deadbeef", &CommitLookup::NotACommit, start);

    let within_ttl = start + NOT_A_COMMIT_TTL - Duration::from_secs(1);
    assert_eq!(
        cache.get(dir, "deadbeef", within_ttl),
        Some(CommitLookup::NotACommit)
    );

    let after_ttl = start + NOT_A_COMMIT_TTL;
    assert_eq!(cache.get(dir, "deadbeef", after_ttl), None);
    assert_eq!(cache.get(dir, "ab5d5f6", after_ttl), Some(commit));
    assert_eq!(cache.get(Path::new("/other"), "ab5d5f6", after_ttl), None);
}

#[test]
fn unknown_outside_repo_does_not_poison_the_same_hash_in_a_repo() {
    let cache = Mutex::new(CommitCache::default());
    let (repo, head) = repo_with_commit("Real");
    let not_repo = TempDir::new().unwrap();

    assert!(matches!(
        lookup_commit_cached(&cache, not_repo.path(), &head[..9]),
        CommitLookup::Unknown(_)
    ));
    assert!(matches!(
        lookup_commit_cached(&cache, not_repo.path(), &head[..9]),
        CommitLookup::Unknown(_)
    ));
    assert!(matches!(
        lookup_commit_cached(&cache, repo.path(), &head[..9]),
        CommitLookup::Commit(_)
    ));
}
