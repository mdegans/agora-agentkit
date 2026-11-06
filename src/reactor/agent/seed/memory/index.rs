//! [`SearchIndex`]: every line of an agent's memory versions, once, for
//! `search`. The runner owns the file (`sealed/history_index.json`) and
//! when to build it; this is the format and the query.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::id::MemoryId;
use super::render::grouped;
use crate::enums::SearchMode;
use crate::govlog::Sha256Hex;

/// The file format [`SearchIndex::to_bytes`] writes
pub const INDEX_FORMAT: u32 = 1;

/// The most line groups a search shows
pub const MAX_GROUPS: usize = 30;

/// The most characters of a line a search shows
pub const MAX_LINE_CHARS: usize = 300;

/// A deduplicated line table, and the lines of each memory, keyed by
/// memory id, oldest first
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchIndex {
    lines: Vec<String>,
    versions: Vec<IndexedVersion>,
    /// Line text → its place in `lines`, rebuilt when it falls behind
    #[serde(skip)]
    lookup: HashMap<String, u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct IndexedVersion {
    id: MemoryId,
    at: DateTime<Utc>,
    /// Places in the line table, in the memory's order
    lines: Vec<u32>,
}

/// The file: the index and its hash
#[derive(Serialize, Deserialize)]
struct IndexFile {
    format: u32,
    sha256: Sha256Hex,
    index: SearchIndex,
}

/// An index file that can't be used; rebuild it
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("not an index file: {0}")]
    Json(#[from] serde_json::Error),
    #[error("index format {0}, not {INDEX_FORMAT}")]
    Format(u32),
    #[error("the index does not match its hash")]
    Mismatch,
}

/// One distinct line that matched, across the versions that hold it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub line: String,
    /// The oldest version holding it
    pub first: (MemoryId, DateTime<Utc>),
    /// The newest version holding it
    pub last: (MemoryId, DateTime<Utc>),
    /// How many versions hold it
    pub count: usize,
}

/// What a search found, newest first
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub query: String,
    /// At most [`MAX_GROUPS`]
    pub hits: Vec<SearchHit>,
    /// Matching lines past [`MAX_GROUPS`]
    pub more: usize,
    /// Versions searched
    pub versions: usize,
}

impl SearchIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Versions indexed
    pub fn len(&self) -> usize {
        self.versions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.versions.is_empty()
    }

    /// Whether `id` is indexed
    pub fn contains(&self, id: &MemoryId) -> bool {
        self.versions.iter().any(|v| v.id == *id)
    }

    /// The memory ids indexed, oldest first
    pub fn ids(&self) -> impl Iterator<Item = &MemoryId> {
        self.versions.iter().map(|v| &v.id)
    }

    /// Add the memory `content`, whose id is `id`, saved `at`; `false`
    /// when that memory is already in. Versions may come in any order
    pub fn add(
        &mut self,
        id: MemoryId,
        at: DateTime<Utc>,
        content: &str,
    ) -> bool {
        if self.contains(&id) {
            return false;
        }
        if self.lookup.len() != self.lines.len() {
            self.lookup = self
                .lines
                .iter()
                .enumerate()
                .map(|(i, l)| (l.clone(), i as u32))
                .collect();
        }
        let mut ids = Vec::new();
        for line in content.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let next = self.lines.len() as u32;
            let at = *self.lookup.entry(line.to_string()).or_insert(next);
            if at == next {
                self.lines.push(line.to_string());
            }
            ids.push(at);
        }
        let place = self.versions.partition_point(|v| v.at <= at);
        self.versions
            .insert(place, IndexedVersion { id, at, lines: ids });
        true
    }

    /// SHA-256 of the line table and the versions
    pub fn content_hash(&self) -> Sha256Hex {
        let body =
            serde_json::to_vec(&(INDEX_FORMAT, &self.lines, &self.versions))
                .expect("an index always serializes");
        Sha256Hex::from(<[u8; 32]>::from(Sha256::digest(body)))
    }

    /// The file's bytes, with the hash
    pub fn to_bytes(&self) -> Vec<u8> {
        #[derive(Serialize)]
        struct Out<'a> {
            format: u32,
            sha256: Sha256Hex,
            index: &'a SearchIndex,
        }
        serde_json::to_vec(&Out {
            format: INDEX_FORMAT,
            sha256: self.content_hash(),
            index: self,
        })
        .expect("an index always serializes")
    }

    /// Read a file [`to_bytes`](Self::to_bytes) wrote, checking its hash
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        let file: IndexFile = serde_json::from_slice(bytes)?;
        if file.format != INDEX_FORMAT {
            return Err(IndexError::Format(file.format));
        }
        if file.index.content_hash() != file.sha256 {
            return Err(IndexError::Mismatch);
        }
        Ok(file.index)
    }

    /// The lines holding every word of `query`, in any case, grouped by
    /// line: newest first, at most [`MAX_GROUPS`]
    pub fn search(&self, query: &str) -> SearchResult {
        let words: Vec<String> =
            query.split_whitespace().map(str::to_lowercase).collect();
        let mut result = SearchResult {
            query: query.trim().to_string(),
            hits: Vec::new(),
            more: 0,
            versions: self.versions.len(),
        };
        if words.is_empty() {
            return result;
        }
        let matched: Vec<bool> = self
            .lines
            .iter()
            .map(|l| {
                let l = l.to_lowercase();
                words.iter().all(|w| l.contains(w.as_str()))
            })
            .collect();
        // Per matched line: oldest version, newest version, count
        let mut found: HashMap<u32, (usize, usize, usize)> = HashMap::new();
        for (v, version) in self.versions.iter().enumerate() {
            let mut seen = std::collections::HashSet::new();
            for &line in &version.lines {
                if matched[line as usize] && seen.insert(line) {
                    let e = found.entry(line).or_insert((v, v, 0));
                    e.1 = v;
                    e.2 += 1;
                }
            }
        }
        let mut groups: Vec<(u32, (usize, usize, usize))> =
            found.into_iter().collect();
        groups.sort_by_key(|g| std::cmp::Reverse((g.1.1, g.1.0, g.0)));
        result.more = groups.len().saturating_sub(MAX_GROUPS);
        let at = |v: usize| (self.versions[v].id, self.versions[v].at);
        result.hits = groups
            .into_iter()
            .take(MAX_GROUPS)
            .map(|(line, (first, last, count))| SearchHit {
                line: self.lines[line as usize].clone(),
                first: at(first),
                last: at(last),
                count,
            })
            .collect();
        result
    }
}

/// `line`, cut to [`MAX_LINE_CHARS`] around the first of `words` in it
fn excerpt(line: &str, words: &[String]) -> String {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= MAX_LINE_CHARS {
        return line.to_string();
    }
    let lower = line.to_lowercase();
    // Where the first word is, in characters (lowercasing can change byte
    // lengths, so count in the lowercased text and clamp)
    let hit = words
        .iter()
        .filter_map(|w| lower.find(w.as_str()))
        .min()
        .map(|b| lower[..b].chars().count())
        .unwrap_or(0)
        .min(chars.len());
    let start = hit
        .saturating_sub(MAX_LINE_CHARS / 3)
        .min(chars.len() - MAX_LINE_CHARS);
    let end = start + MAX_LINE_CHARS;
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(&chars[start..end]);
    if end < chars.len() {
        out.push('…');
    }
    out
}

impl SearchResult {
    /// What the agent reads. `mode` is the call's: semantic search is
    /// not available for memory, so it is said to have run by keyword
    pub fn render(&self, mode: Option<SearchMode>) -> String {
        let mut out = String::new();
        if mode == Some(SearchMode::Semantic) {
            out.push_str(
                "Semantic search isn't available for your memory yet, so \
                 this was a keyword search.\n\n",
            );
        }
        let versions = match self.versions {
            1 => "your memory's 1 version".to_string(),
            n => format!("your memory's {} versions", grouped(n)),
        };
        if self.hits.is_empty() {
            out.push_str(&format!(
                "No line in {versions} contains every word of \"{}\".",
                self.query
            ));
            return out;
        }
        let words: Vec<String> = self
            .query
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        out.push_str(&format!(
            "Lines in {versions} with every word of \"{}\", newest first:",
            self.query
        ));
        for hit in &self.hits {
            let date = |at: DateTime<Utc>| at.format("%Y-%m-%d").to_string();
            let span = if hit.count == 1 {
                format!("only in {} ({})", hit.last.0, date(hit.last.1))
            } else {
                format!(
                    "in {} versions, from {} ({}) to {} ({})",
                    hit.count,
                    hit.first.0,
                    date(hit.first.1),
                    hit.last.0,
                    date(hit.last.1)
                )
            };
            out.push_str(&format!(
                "\n- {}\n  {span}",
                excerpt(&hit.line, &words)
            ));
        }
        if self.more > 0 {
            out.push_str(&format!(
                "\n{} more line{} match; narrow the query.",
                self.more,
                if self.more == 1 { "" } else { "s" }
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn id(n: u8) -> MemoryId {
        MemoryId::from([n; 32])
    }

    fn at(day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, day, 12, 0, 0).unwrap()
    }

    fn index() -> SearchIndex {
        let mut ix = SearchIndex::new();
        // Out of order, as a backfill may come
        assert!(ix.add(
            id(3),
            at(3),
            "# Memory\n\nThe anchor is ANC-5.\nA thread on Haiku.\n"
        ));
        assert!(ix.add(id(1), at(1), "# Memory\nthe ANCHOR was late\n"));
        assert!(ix.add(
            id(2),
            at(2),
            "# Memory\n  The anchor is ANC-5.  \nthe anchor was late\n"
        ));
        assert!(!ix.add(id(2), at(4), "anything"));
        ix
    }

    #[test]
    fn lines_are_deduplicated_and_versions_ordered() {
        let ix = index();
        assert_eq!(ix.len(), 3);
        assert_eq!(ix.lines.len(), 5);
        assert_eq!(
            ix.ids().copied().collect::<Vec<_>>(),
            [id(1), id(2), id(3)]
        );
        assert!(ix.contains(&id(1)) && !ix.contains(&id(9)));
    }

    #[test]
    fn search_groups_by_line_newest_first() {
        let r = index().search("anchor ANC-5");
        assert_eq!(r.versions, 3);
        assert_eq!(r.more, 0);
        assert_eq!(
            r.hits,
            [SearchHit {
                line: "The anchor is ANC-5.".into(),
                first: (id(2), at(2)),
                last: (id(3), at(3)),
                count: 2,
            }]
        );
        let r = index().search("anchor");
        // Lines differing only in case are distinct lines
        assert_eq!(r.hits.len(), 3);
        assert_eq!(r.hits[0].line, "The anchor is ANC-5.");
        assert_eq!(r.hits[1].line, "the anchor was late");
        assert_eq!(r.hits[2].line, "the ANCHOR was late");
        assert_eq!(r.hits[2].first, (id(1), at(1)));
        assert_eq!(r.hits[2].last, (id(1), at(1)));

        let text = r.render(None);
        assert!(text.starts_with(
            "Lines in your memory's 3 versions with every word of \"anchor\", newest first:"
        ), "{text}");
        assert!(text.contains(&format!(
            "- The anchor is ANC-5.\n  in 2 versions, from {} (2026-11-02) to {} (2026-11-03)",
            id(2),
            id(3)
        )), "{text}");
        let one = index().search("haiku").render(Some(SearchMode::Semantic));
        assert!(one.starts_with("Semantic search isn't available"), "{one}");
        assert!(
            one.ends_with(&format!("only in {} (2026-11-03)", id(3))),
            "{one}"
        );
        let none = index().search("zebra").render(None);
        assert_eq!(
            none,
            "No line in your memory's 3 versions contains every word of \"zebra\"."
        );
        assert!(index().search("  ").hits.is_empty());
    }

    #[test]
    fn past_the_cap_the_rest_are_counted() {
        let mut ix = SearchIndex::new();
        let content: String = (0..45).map(|i| format!("note {i}\n")).collect();
        ix.add(id(1), at(1), &content);
        let r = ix.search("note");
        assert_eq!(r.hits.len(), MAX_GROUPS);
        assert_eq!(r.more, 15);
        assert!(
            r.render(None)
                .ends_with("\n15 more lines match; narrow the query.")
        );
    }

    #[test]
    fn long_lines_are_cut_around_the_match() {
        let line = format!("{}needle{}", "a".repeat(500), "b".repeat(500));
        let out = excerpt(&line, &["needle".into()]);
        assert!(out.contains("needle"));
        assert!(out.starts_with('…') && out.ends_with('…'));
        assert_eq!(out.chars().count(), MAX_LINE_CHARS + 2);
        assert_eq!(excerpt("short", &[]), "short");
    }

    #[test]
    fn the_file_round_trips_and_is_hash_checked() {
        let ix = index();
        let bytes = ix.to_bytes();
        let mut back = SearchIndex::from_bytes(&bytes).unwrap();
        assert_eq!(back.to_bytes(), bytes);
        assert_eq!(back.content_hash(), ix.content_hash());
        // Adding after a load still deduplicates
        assert!(back.add(id(4), at(5), "The anchor is ANC-5.\nnew line\n"));
        assert_eq!(back.lines.len(), 6);

        // Plain serde too
        let json = serde_json::to_string(&ix).unwrap();
        let plain: SearchIndex = serde_json::from_str(&json).unwrap();
        assert_eq!(plain.to_bytes(), bytes);

        let tampered = String::from_utf8(bytes.clone())
            .unwrap()
            .replace("the ANCHOR was late", "the anchor was early");
        assert!(matches!(
            SearchIndex::from_bytes(tampered.as_bytes()),
            Err(IndexError::Mismatch)
        ));
        let other = String::from_utf8(bytes).unwrap().replacen(
            "\"format\":1",
            "\"format\":2",
            1,
        );
        assert!(matches!(
            SearchIndex::from_bytes(other.as_bytes()),
            Err(IndexError::Format(2))
        ));
        assert!(matches!(
            SearchIndex::from_bytes(b"{"),
            Err(IndexError::Json(_))
        ));
    }
}
