//! Time Machine: a git snapshot of the working tree after every turn that
//! changed files, kept in a private index (`<git-dir>/blush-timeline.index`)
//! so the user's staging area, HEAD and branches are never touched.
//!
//! Snapshots are commits referenced from `refs/blush/timeline/<session>`;
//! jumping restores only the paths that differ between two snapshots.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Local};

#[derive(Clone, Debug)]
pub struct Snap {
    pub commit: String,
    pub tree: String,
    pub label: String,
    pub time: DateTime<Local>,
    /// Files changed relative to the previous snapshot.
    pub changed: usize,
}

#[derive(Default)]
pub struct Timeline {
    repo: Option<Repo>,
    pub snaps: Vec<Snap>,
    /// Index into `snaps` of the state the working tree is in.
    pub cur: usize,
    probed: bool,
    ref_name: String,
}

#[derive(Clone)]
struct Repo {
    top: PathBuf,
    index: PathBuf,
}

impl Timeline {
    pub fn available(&self) -> bool {
        self.repo.is_some()
    }

    fn ensure_repo(&mut self, cwd: &Path) -> Option<Repo> {
        if !self.probed {
            self.probed = true;
            let top = git(cwd, None, &["rev-parse", "--show-toplevel"]).ok();
            let dir = git(cwd, None, &["rev-parse", "--absolute-git-dir"]).ok();
            if let (Some(top), Some(dir)) = (top, dir) {
                let top = PathBuf::from(top.trim());
                let index = PathBuf::from(dir.trim()).join("blush-timeline.index");
                self.repo = Some(Repo { top, index });
            }
            // The pid keeps two sessions started in the same second apart.
            self.ref_name = format!("refs/blush/timeline/{}-{}", Local::now().format("%Y%m%d-%H%M%S"), std::process::id());
        }
        self.repo.clone()
    }

    /// Record the working tree if it differs from the current snapshot.
    /// Returns the new snapshot, if one was taken.
    pub fn snapshot(&mut self, cwd: &Path, label: &str) -> Result<Option<Snap>> {
        let Some(repo) = self.ensure_repo(cwd) else { return Ok(None) };
        let tree = write_tree(&repo)?;
        let last = self.snaps.get(self.cur);
        if last.is_some_and(|s| s.tree == tree) {
            return Ok(None);
        }
        let changed = match last {
            Some(prev) => changed_paths(&repo, &prev.tree, &tree)?.len(),
            None => 0,
        };
        let mut args = vec!["commit-tree".to_string(), tree.clone(), "-m".into(), format!("blush: {label}")];
        if let Some(parent) = self.snaps.last() {
            args.push("-p".into());
            args.push(parent.commit.clone());
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let commit = git(&repo.top, None, &args)?.trim().to_string();
        git(&repo.top, None, &["update-ref", &self.ref_name, &commit])?;
        let snap = Snap { commit, tree, label: label.to_string(), time: Local::now(), changed };
        // Never discard history: states after `cur` stay reachable in the list.
        self.snaps.push(snap.clone());
        self.cur = self.snaps.len() - 1;
        Ok(Some(snap))
    }

    /// Restore the working tree to snapshot `to`. Unsaved changes are
    /// snapshotted first so the jump can always be undone.
    pub fn jump(&mut self, cwd: &Path, to: usize) -> Result<usize> {
        let Some(repo) = self.ensure_repo(cwd) else { bail!("not a git repository") };
        if to >= self.snaps.len() {
            bail!("no such snapshot");
        }
        self.snapshot(cwd, "unsaved changes")?;
        let from_tree = self.snaps[self.cur].tree.clone();
        let target = self.snaps[to].clone();
        let changes = changed_paths(&repo, &from_tree, &target.tree)?;
        let mut restore = Vec::new();
        for (status, path) in &changes {
            if status == "A" {
                // Present now, absent in the target state.
                let p = repo.top.join(path);
                let _ = std::fs::remove_file(&p);
                prune_empty_dirs(&repo.top, p.parent());
            } else {
                restore.push(path.clone());
            }
        }
        git(&repo.top, Some(&repo.index), &["read-tree", &target.tree])?;
        if !restore.is_empty() {
            let mut input = Vec::new();
            for p in &restore {
                input.extend_from_slice(p.as_bytes());
                input.push(0);
            }
            git_stdin(&repo.top, Some(&repo.index), &["checkout-index", "-f", "-z", "--stdin"], &input)?;
        }
        self.cur = to;
        Ok(changes.len())
    }

    /// `git diff --stat`-style summary between a snapshot and the one before it.
    pub fn diff_stat(&mut self, cwd: &Path, idx: usize) -> Result<String> {
        let Some(repo) = self.ensure_repo(cwd) else { bail!("not a git repository") };
        let snap = self.snaps.get(idx).ok_or_else(|| anyhow!("no such snapshot"))?;
        let Some(prev) = idx.checked_sub(1).and_then(|i| self.snaps.get(i)) else {
            return Ok("Starting state".into());
        };
        git(&repo.top, None, &["diff", "--stat", "--no-color", &prev.tree, &snap.tree])
    }

    /// Full patch between a snapshot and the one before it.
    pub fn diff(&mut self, cwd: &Path, idx: usize) -> Result<String> {
        let Some(repo) = self.ensure_repo(cwd) else { bail!("not a git repository") };
        let snap = self.snaps.get(idx).ok_or_else(|| anyhow!("no such snapshot"))?;
        let Some(prev) = idx.checked_sub(1).and_then(|i| self.snaps.get(i)) else {
            return Ok("Starting state — nothing to compare against.".into());
        };
        git(&repo.top, None, &["diff", "--no-color", &prev.tree, &snap.tree])
    }
}

fn write_tree(repo: &Repo) -> Result<String> {
    git(&repo.top, Some(&repo.index), &["add", "-A", "--", "."])?;
    Ok(git(&repo.top, Some(&repo.index), &["write-tree"])?.trim().to_string())
}

/// (status, path) for every path that differs between tree `a` (where we
/// are) and tree `b` (where we're going). Status "A" means the path exists
/// only in `a`, so reaching `b` means deleting it; anything else is restored
/// from `b`.
fn changed_paths(repo: &Repo, a: &str, b: &str) -> Result<Vec<(String, String)>> {
    // `diff-tree b a` describes a relative to b, so "A" = present in a only.
    let out = git(&repo.top, None, &["diff-tree", "-r", "--no-renames", "--name-status", "-z", b, a])?;
    let mut parts = out.split('\0').filter(|s| !s.is_empty());
    let mut v = Vec::new();
    while let (Some(st), Some(path)) = (parts.next(), parts.next()) {
        v.push((st.to_string(), path.to_string()));
    }
    Ok(v)
}

fn prune_empty_dirs(top: &Path, mut dir: Option<&Path>) {
    while let Some(d) = dir {
        if d == top || !d.starts_with(top) || std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

fn command(cwd: &Path, index: Option<&Path>, args: &[&str]) -> Command {
    let mut c = Command::new("git");
    c.current_dir(cwd).args(args).env("GIT_TERMINAL_PROMPT", "0");
    if let Some(i) = index {
        c.env("GIT_INDEX_FILE", i);
    }
    // Snapshots must not depend on the user's identity being configured.
    c.env("GIT_AUTHOR_NAME", "blush")
        .env("GIT_AUTHOR_EMAIL", "blush@localhost")
        .env("GIT_COMMITTER_NAME", "blush")
        .env("GIT_COMMITTER_EMAIL", "blush@localhost");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

fn git(cwd: &Path, index: Option<&Path>, args: &[&str]) -> Result<String> {
    let out = command(cwd, index, args).stdin(Stdio::null()).output()?;
    if !out.status.success() {
        bail!("git {}: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn git_stdin(cwd: &Path, index: Option<&Path>, args: &[&str], input: &[u8]) -> Result<String> {
    let mut child = command(cwd, index, args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    child.stdin.take().unwrap().write_all(input)?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("git {}: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_repo() -> Option<PathBuf> {
        let dir = std::env::temp_dir().join(format!("blush-tl-{}-{}", std::process::id(), rand_suffix()));
        std::fs::create_dir_all(&dir).ok()?;
        git(&dir, None, &["init", "-q"]).ok()?;
        Some(dunce(dir))
    }

    fn dunce(p: PathBuf) -> PathBuf {
        std::fs::canonicalize(&p)
            .map(|c| {
                let s = c.to_string_lossy().to_string();
                PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s))
            })
            .unwrap_or(p)
    }

    fn rand_suffix() -> u128 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    }

    #[test]
    fn snapshot_and_jump_round_trip() {
        let Some(dir) = tmp_repo() else { return }; // git not installed
        std::fs::write(dir.join("a.txt"), "one").unwrap();
        let mut tl = Timeline::default();
        assert!(tl.snapshot(&dir, "start").unwrap().is_some());
        assert!(tl.snapshot(&dir, "same").unwrap().is_none(), "unchanged tree makes no snapshot");

        std::fs::write(dir.join("a.txt"), "two").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/b.txt"), "new").unwrap();
        let s = tl.snapshot(&dir, "turn 1").unwrap().unwrap();
        assert_eq!(s.changed, 2);

        tl.jump(&dir, 0).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one");
        assert!(!dir.join("sub/b.txt").exists());
        assert!(!dir.join("sub").exists(), "emptied directories are pruned");

        tl.jump(&dir, 1).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "two");
        assert_eq!(std::fs::read_to_string(dir.join("sub/b.txt")).unwrap(), "new");

        // Unsaved edits are kept as their own state before jumping away.
        std::fs::write(dir.join("a.txt"), "three").unwrap();
        tl.jump(&dir, 0).unwrap();
        assert_eq!(tl.snaps.len(), 3);
        tl.jump(&dir, 2).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "three");

        // The user's real index and HEAD are untouched.
        let status = git(&dir, None, &["status", "--porcelain"]).unwrap();
        assert!(status.lines().all(|l| l.starts_with("??")), "nothing staged: {status}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
