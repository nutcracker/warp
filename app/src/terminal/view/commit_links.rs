//! Detection of git commit hashes in terminal output, confirmed against the repository the
//! output came from.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use command::Stdio;
use command::blocking::Command;
use instant::Instant;

use crate::terminal::model::grid::grid_handler::{ContainsPoint, Link};
use crate::terminal::model::index::Point;

const MIN_HASH_LEN: usize = 7;
const MAX_HASH_LEN: usize = 40;
const GIT_TIMEOUT: Duration = Duration::from_secs(2);
const GIT_POLL_INTERVAL: Duration = Duration::from_millis(5);
/// A word that isn't a commit can become one after a `git fetch`, so negative results expire.
const NOT_A_COMMIT_TTL: Duration = Duration::from_secs(60);
const MAX_CACHE_ENTRIES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitInfo {
    pub full_hash: String,
    pub subject: String,
    pub body: String,
    pub author: String,
    /// Author date, in seconds since the Unix epoch.
    pub timestamp: i64,
    /// The commit's page on the `origin` remote's web host, when that host is recognised.
    pub web_url: Option<String>,
}

/// The result of asking git whether a word names a commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitLookup {
    Commit(CommitInfo),
    NotACommit,
    /// Git could not give an answer (not a repository, an ambiguous prefix, a timeout, git
    /// missing). Must never be cached, so a transient failure can't silence a real commit.
    Unknown(String),
}

/// A commit hash found in the grid, with the commit it resolved to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitLink {
    pub link: Link,
    pub info: CommitInfo,
}

impl ContainsPoint for CommitLink {
    fn contains(&self, point: Point) -> bool {
        self.link.contains(point)
    }
}

/// A hex word inside a hovered fragment, with the number of punctuation characters trimmed
/// from each end of the fragment to reach it.
#[derive(Debug, PartialEq, Eq)]
pub struct CommitCandidate<'a> {
    pub hash: &'a str,
    pub leading_trim: usize,
    pub trailing_trim: usize,
}

/// Returns the commit-hash-shaped word in `fragment`, if there is one: 7–40 lowercase hex
/// characters once surrounding ASCII punctuation (`.`, `*`, `#`, …) is removed. Git prints
/// hashes in lowercase, so uppercase hex is treated as some other identifier.
pub fn commit_candidate(fragment: &str) -> Option<CommitCandidate<'_>> {
    let start_trimmed = fragment.trim_start_matches(|c: char| c.is_ascii_punctuation());
    let hash = start_trimmed.trim_end_matches(|c: char| c.is_ascii_punctuation());
    let is_hash = (MIN_HASH_LEN..=MAX_HASH_LEN).contains(&hash.len())
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    is_hash.then(|| CommitCandidate {
        hash,
        leading_trim: fragment.len() - start_trimmed.len(),
        trailing_trim: start_trimmed.len() - hash.len(),
    })
}

/// Asks git, in `repo_dir`, whether `candidate` names a commit, and if so fetches its details.
pub fn lookup_commit(repo_dir: &Path, candidate: &str) -> CommitLookup {
    let verify_arg = format!("{candidate}^{{commit}}");
    // `--quiet` is deliberately not used: it reports an ambiguous prefix with the same silent
    // exit status as a missing object, and the two must be told apart.
    let full_hash = match run_git(repo_dir, &["rev-parse", "--verify", &verify_arg]) {
        Ok(output) => match classify_rev_parse(&output) {
            RevParse::Commit(full_hash) => full_hash,
            RevParse::NotACommit => return CommitLookup::NotACommit,
            RevParse::Unknown(reason) => return CommitLookup::Unknown(reason),
        },
        Err(reason) => return CommitLookup::Unknown(reason),
    };

    let show = match run_git(
        repo_dir,
        &[
            "-c",
            "log.showSignature=false",
            "show",
            "-s",
            "--format=%H%x00%an%x00%at%x00%s%x00%b",
            &full_hash,
        ],
    ) {
        Ok(output) if output.success() => output,
        Ok(output) => return CommitLookup::Unknown(output.failure_reason("git show")),
        Err(reason) => return CommitLookup::Unknown(reason),
    };
    let Some(mut info) = parse_show_output(&show.stdout) else {
        return CommitLookup::Unknown("unparseable git show output".to_owned());
    };

    // `git config --get` exits 1 when the key is unset, which is an answer: no remote.
    info.web_url = match run_git(repo_dir, &["config", "--get", "remote.origin.url"]) {
        Ok(output) if output.success() => commit_web_url(output.stdout.trim(), &info.full_hash),
        Ok(output) if output.exit_code == Some(1) => None,
        Ok(output) => return CommitLookup::Unknown(output.failure_reason("git config")),
        Err(reason) => return CommitLookup::Unknown(reason),
    };

    CommitLookup::Commit(info)
}

#[derive(Debug, PartialEq, Eq)]
enum RevParse {
    Commit(String),
    NotACommit,
    Unknown(String),
}

fn classify_rev_parse(output: &GitOutput) -> RevParse {
    if output.success() {
        let hash = output.stdout.trim();
        return if hash.len() == MAX_HASH_LEN {
            RevParse::Commit(hash.to_owned())
        } else {
            RevParse::Unknown(format!("unexpected git rev-parse output {hash:?}"))
        };
    }
    if output.stderr.contains("is ambiguous") {
        return RevParse::Unknown("ambiguous short hash".to_owned());
    }
    // With LC_ALL=C this is git's only message for a name that resolves to no commit, whether
    // no object matches or the object is a tree or blob.
    if output.exit_code == Some(128)
        && output.stderr.trim_end().lines().last() == Some("fatal: Needed a single revision")
    {
        return RevParse::NotACommit;
    }
    RevParse::Unknown(output.failure_reason("git rev-parse"))
}

fn parse_show_output(stdout: &str) -> Option<CommitInfo> {
    let mut fields = stdout.splitn(5, '\0');
    let full_hash = fields.next()?.trim().to_owned();
    let author = fields.next()?.to_owned();
    let timestamp = fields.next()?.parse().ok()?;
    let subject = fields.next()?.to_owned();
    let body = fields.next()?.trim().to_owned();
    Some(CommitInfo {
        full_hash,
        subject,
        body,
        author,
        timestamp,
        web_url: None,
    })
}

/// Maps an `origin` URL (SSH, scp-style or HTTPS) to the web page for `full_hash`, for hosts
/// whose commit URL shape is known.
pub fn commit_web_url(origin: &str, full_hash: &str) -> Option<String> {
    let (host, path) = remote_host_and_path(origin)?;
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    if path.is_empty() {
        return None;
    }
    match host.as_str() {
        "github.com" => Some(format!("https://github.com/{path}/commit/{full_hash}")),
        "bitbucket.org" => Some(format!("https://bitbucket.org/{path}/commits/{full_hash}")),
        "gitlab.com" => Some(format!("https://gitlab.com/{path}/-/commit/{full_hash}")),
        host if host.starts_with("gitlab.") => {
            Some(format!("https://{host}/{path}/-/commit/{full_hash}"))
        }
        _ => None,
    }
}

fn remote_host_and_path(origin: &str) -> Option<(String, String)> {
    if origin.contains("://") {
        let url = url::Url::parse(origin).ok()?;
        if !matches!(url.scheme(), "https" | "http" | "ssh" | "git") {
            return None;
        }
        return Some((url.host_str()?.to_ascii_lowercase(), url.path().to_owned()));
    }
    // scp-style: [user@]host:path
    let (authority, path) = origin.split_once(':')?;
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if host.is_empty() || host.contains('/') {
        return None;
    }
    Some((host.to_ascii_lowercase(), path.to_owned()))
}

struct GitOutput {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl GitOutput {
    fn success(&self) -> bool {
        self.exit_code == Some(0)
    }

    fn failure_reason(&self, command: &str) -> String {
        format!(
            "{command} exited with {:?}: {}",
            self.exit_code,
            self.stderr.trim()
        )
    }
}

/// Runs git with a hard timeout. `Err` means no exit status was obtained at all.
fn run_git(repo_dir: &Path, args: &[&str]) -> Result<GitOutput, String> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(repo_dir)
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn git: {e}"))?;

    // Drain both pipes concurrently so a large commit message can't fill a pipe and stall git.
    let stdout_reader = child.stdout.take().map(spawn_pipe_reader);
    let stderr_reader = child.stderr.take().map(spawn_pipe_reader);

    let deadline = Instant::now() + GIT_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("git {} timed out", args.join(" ")));
            }
            Ok(None) => thread::sleep(GIT_POLL_INTERVAL),
            Err(e) => return Err(format!("failed to wait for git: {e}")),
        }
    };

    let collect = |reader: Option<thread::JoinHandle<Vec<u8>>>| {
        reader
            .and_then(|handle| handle.join().ok())
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default()
    };
    Ok(GitOutput {
        exit_code: status.code(),
        stdout: collect(stdout_reader),
        stderr: collect(stderr_reader),
    })
}

fn spawn_pipe_reader(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        bytes
    })
}

/// Results of [`lookup_commit`], keyed by the directory asked in and the candidate word.
#[derive(Default)]
pub struct CommitCache {
    /// Commits are immutable, so a found commit is kept for the session.
    found: HashMap<(PathBuf, String), CommitInfo>,
    /// When each word was found not to be a commit.
    absent: HashMap<(PathBuf, String), Instant>,
}

impl CommitCache {
    pub fn get(&mut self, repo_dir: &Path, candidate: &str, now: Instant) -> Option<CommitLookup> {
        let key = (repo_dir.to_path_buf(), candidate.to_owned());
        if let Some(info) = self.found.get(&key) {
            return Some(CommitLookup::Commit(info.clone()));
        }
        match self.absent.get(&key) {
            Some(recorded) if now.saturating_duration_since(*recorded) < NOT_A_COMMIT_TTL => {
                Some(CommitLookup::NotACommit)
            }
            Some(_) => {
                self.absent.remove(&key);
                None
            }
            None => None,
        }
    }

    pub fn record(
        &mut self,
        repo_dir: &Path,
        candidate: &str,
        lookup: &CommitLookup,
        now: Instant,
    ) {
        let key = (repo_dir.to_path_buf(), candidate.to_owned());
        match lookup {
            CommitLookup::Commit(info) => {
                if self.found.len() >= MAX_CACHE_ENTRIES {
                    self.found.clear();
                }
                self.found.insert(key, info.clone());
            }
            CommitLookup::NotACommit => {
                if self.absent.len() >= MAX_CACHE_ENTRIES {
                    self.absent.clear();
                }
                self.absent.insert(key, now);
            }
            CommitLookup::Unknown(_) => {}
        }
    }
}

/// [`lookup_commit`] through `cache`. The lock is not held while git runs.
pub fn lookup_commit_cached(
    cache: &Mutex<CommitCache>,
    repo_dir: &Path,
    candidate: &str,
) -> CommitLookup {
    if let Some(cached) = cache
        .lock()
        .ok()
        .and_then(|mut cache| cache.get(repo_dir, candidate, Instant::now()))
    {
        return cached;
    }
    let lookup = lookup_commit(repo_dir, candidate);
    if let Ok(mut cache) = cache.lock() {
        cache.record(repo_dir, candidate, &lookup, Instant::now());
    }
    lookup
}

#[cfg(test)]
#[path = "commit_links_tests.rs"]
mod tests;
