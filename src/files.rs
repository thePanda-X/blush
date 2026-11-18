//! Background file index for `@` mentions, with fuzzy matching.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

const MAX_FILES: usize = 60_000;

#[derive(Clone, Default)]
pub struct FileIndex {
    files: Arc<RwLock<Vec<String>>>,
}

impl FileIndex {
    pub fn spawn(root: PathBuf) -> FileIndex {
        let idx = FileIndex::default();
        let files = idx.files.clone();
        std::thread::spawn(move || {
            let mut out = Vec::new();
            let walker = ignore::WalkBuilder::new(&root)
                .hidden(false)
                .filter_entry(|e| {
                    let n = e.file_name().to_string_lossy();
                    !(n == ".git" || n == "node_modules" || n == "target")
                })
                .build();
            for entry in walker.flatten() {
                if out.len() >= MAX_FILES {
                    break;
                }
                let p = entry.path();
                if p == root {
                    continue;
                }
                if let Ok(rel) = p.strip_prefix(&root) {
                    let mut s = rel.to_string_lossy().replace('\\', "/");
                    if entry.file_type().is_some_and(|t| t.is_dir()) {
                        s.push('/');
                    }
                    out.push(s);
                }
            }
            out.sort_by_key(|s| (s.matches('/').count(), s.len()));
            if let Ok(mut f) = files.write() {
                *f = out;
            }
        });
        idx
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<String> {
        let Ok(files) = self.files.read() else { return vec![] };
        if query.is_empty() {
            return files.iter().take(limit).cloned().collect();
        }
        let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        let mut buf = Vec::new();
        let mut scored: Vec<(u32, &String)> = files
            .iter()
            .filter_map(|f| {
                pattern.score(Utf32Str::new(f, &mut buf), &mut matcher).map(|s| (s, f))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.len().cmp(&b.1.len())));
        scored.into_iter().take(limit).map(|(_, f)| f.clone()).collect()
    }
}

/// Fuzzy-filter arbitrary labels; returns indices sorted by score.
pub fn fuzzy_filter(query: &str, items: &[String]) -> Vec<usize> {
    if query.is_empty() {
        return (0..items.len()).collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, usize)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, s)| pattern.score(Utf32Str::new(s, &mut buf), &mut matcher).map(|sc| (sc, i)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, i)| i).collect()
}

/// Resolve `@path` tokens in a prompt to existing files under `cwd`.
pub fn mentioned_paths(text: &str, cwd: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for tok in text.split_whitespace() {
        let Some(rest) = tok.strip_prefix('@') else { continue };
        let rest = rest.trim_end_matches([',', '.', ';', ':', ')', '"', '\'']);
        if rest.is_empty() {
            continue;
        }
        let p = cwd.join(rest);
        if p.exists() && !out.contains(&p) {
            out.push(p);
        }
    }
    out
}
