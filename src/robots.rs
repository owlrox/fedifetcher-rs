//! A port of CPython's `urllib.robotparser.RobotFileParser`, so that
//! robots.txt decisions match the Python original exactly.
//!
//! The robots.txt Exclusion Protocol is implemented as specified in RFC 9309.
//! Only `parse` and `can_fetch` are used by FediFetcher; the wildcard matching
//! mirrors CPython's, including its quirks: `*` runs are matched leftmost for
//! every literal but the last, which is matched rightmost, because CPython
//! compiles the last segment as a greedy `.*`.

/// One parsed robots.txt file.
pub struct RobotFileParser {
    entries: Vec<Entry>,
    /// Maps each lowercased user-agent token to the (possibly merged) entry.
    groups: indexmap::IndexMap<String, Entry>,
    disallow_all: bool,
    allow_all: bool,
    last_checked: bool,
}

#[derive(Clone, Default)]
struct Entry {
    useragents: Vec<String>,
    rulelines: Vec<RuleLine>,
}

#[derive(Clone)]
struct RuleLine {
    path: String,
    allowance: bool,
    fullmatch: bool,
    /// The literal segments between the `*`s, when the path contains any.
    wildcard_parts: Option<Vec<String>>,
}

impl RobotFileParser {
    pub fn new() -> Self {
        RobotFileParser {
            entries: Vec::new(),
            groups: indexmap::IndexMap::new(),
            disallow_all: false,
            allow_all: false,
            last_checked: false,
        }
    }

    fn add_entry(&mut self, entry: Entry) {
        for agent in &entry.useragents {
            let agent = agent.to_lowercase();
            match self.groups.get_mut(&agent) {
                Some(existing) => *existing = merge_entries(existing, &entry),
                None => {
                    self.groups.insert(agent, entry.clone());
                }
            }
        }
        self.entries.push(entry);
    }

    /// Parse the lines of a robots.txt file.
    ///
    /// We allow that a user-agent: line is not preceded by one or more blank
    /// lines.
    pub fn parse<I: IntoIterator<Item = String>>(&mut self, lines: I) {
        // states: 0: start state, 1: saw user-agent line, 2: saw an allow or
        // disallow line
        let mut state = 0;
        let mut entry = Entry::default();

        self.last_checked = true;
        for line in lines {
            // remove optional comment and strip line
            let line = match line.find('#') {
                Some(i) => &line[..i],
                None => &line[..],
            };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((field, value)) => (field.trim().to_lowercase(), value.trim().to_string()),
                None => continue,
            };
            match field.as_str() {
                "user-agent" => {
                    if state == 2 {
                        self.add_entry(entry);
                        entry = Entry::default();
                    }
                    entry.useragents.push(value);
                    state = 1;
                }
                "disallow" => {
                    if state != 0 {
                        state = 2;
                        if let Ok(rule) = RuleLine::new(value, false) {
                            entry.rulelines.push(rule);
                        }
                    }
                }
                "allow" if state != 0 => {
                    state = 2;
                    if let Ok(rule) = RuleLine::new(value, true) {
                        entry.rulelines.push(rule);
                    }
                }
                // crawl-delay, request-rate and sitemap are parsed by the
                // Python original but never asked about; they are skipped here
                _ => {}
            }
        }
        if state != 0 {
            self.add_entry(entry);
        }
    }

    fn find_entry(&self, useragent: &str) -> Option<&Entry> {
        if let Some(entry) = self.groups.get(&useragent.to_lowercase()) {
            return Some(entry);
        }
        for entry in self.groups.values() {
            if entry.applies_to(useragent) {
                return Some(entry);
            }
        }
        self.groups.get("*")
    }

    /// Using the parsed robots.txt, decide if useragent can fetch url.
    pub fn can_fetch(&self, useragent: &str, url: &str) -> bool {
        if self.disallow_all {
            return false;
        }
        if self.allow_all {
            return true;
        }
        // Until the robots.txt file has been read, we must assume that no url
        // is allowable. This prevents false positives when can_fetch() is
        // called before parse().
        if !self.last_checked {
            return false;
        }
        // Drop the scheme, netloc and fragment, keeping path and query.
        let path_query = extract_path_query(url);
        let normalized = normalize_uri(&path_query);
        let normalized = if normalized.is_empty() {
            "/".to_string()
        } else {
            normalized
        };
        if normalized == "/robots.txt" {
            // The /robots.txt URI is implicitly allowed.
            return true;
        }
        match self.find_entry(useragent) {
            None => true,
            Some(entry) => entry.allowance(&normalized),
        }
    }
}

impl Default for RobotFileParser {
    fn default() -> Self {
        Self::new()
    }
}

impl RuleLine {
    /// A rule line is a single `Allow:` (allowance == true) or `Disallow:`
    /// (allowance == false) followed by a path.
    fn new(path: String, allowance: bool) -> Result<RuleLine, ()> {
        // an empty value means allow all
        let allowance = allowance || path.is_empty();
        let path = collapse_stars(&path);
        let path = collapse_dollars(&path);
        let path = normalize_pattern(&path);
        let fullmatch = path.ends_with('$');
        let path = path.trim_end_matches('$').to_string();
        if path.contains('$') {
            return Err(());
        }
        let wildcard_parts = if path.contains('*') {
            Some(path.split('*').map(str::to_string).collect())
        } else {
            None
        };
        Ok(RuleLine {
            path,
            allowance,
            fullmatch,
            wildcard_parts,
        })
    }

    /// If the filename matches the rule, return the matching length plus 1.
    /// If it does not match, return 0.
    fn applies_to(&self, filename: &str) -> usize {
        match &self.wildcard_parts {
            Some(parts) => self.applies_wildcard(filename, parts),
            None => {
                if self.fullmatch {
                    if filename == self.path {
                        self.path.len() + 1
                    } else {
                        0
                    }
                } else if filename.starts_with(&self.path) {
                    self.path.len() + 1
                } else {
                    0
                }
            }
        }
    }

    /// Match `L0 (?>.*?L1) ... .*Lk` against the start of `filename`, the way
    /// CPython's compiled pattern does, and return the match end plus 1.
    fn applies_wildcard(&self, filename: &str, parts: &[String]) -> usize {
        let first = &parts[0];
        if !filename.starts_with(first.as_str()) {
            return 0;
        }
        let mut pos = first.len();
        let last = parts.len() - 1;
        // Every literal but the last is matched leftmost, because CPython
        // compiles them as non-backtracking lazy segments.
        for part in &parts[1..last] {
            match find_from(filename, part, pos) {
                Some(at) => pos = at + part.len(),
                None => return 0,
            }
        }
        let tail = &parts[last];
        if self.fullmatch {
            // The whole string must be consumed, so the last literal has to
            // end exactly at the end, at or after where we got to.
            if tail.is_empty() {
                return filename.len() + 1;
            }
            if tail.len() > filename.len() {
                return 0;
            }
            let start = filename.len() - tail.len();
            if start >= pos && filename.ends_with(tail.as_str()) {
                filename.len() + 1
            } else {
                0
            }
        } else {
            // The last literal is compiled as a greedy `.*`, so it matches at
            // its rightmost occurrence.
            let end = if tail.is_empty() {
                filename.len()
            } else {
                match rfind_from(filename, tail, pos) {
                    Some(at) => at + tail.len(),
                    None => return 0,
                }
            };
            end + 1
        }
    }
}

impl Entry {
    /// Check if this entry applies to the specified agent.
    fn applies_to(&self, useragent: &str) -> bool {
        // split the name token and make it lower case
        let useragent = useragent
            .split('/')
            .next()
            .unwrap_or_default()
            .to_lowercase();
        self.useragents
            .iter()
            .any(|agent| agent != "*" && useragent.contains(&agent.to_lowercase()))
    }

    fn allowance(&self, filename: &str) -> bool {
        // Preconditions: our agent applies to this entry; filename is URL
        // encoded.
        let mut best_match: i64 = -1;
        let mut allowance = true;
        for line in &self.rulelines {
            let matched = line.applies_to(filename) as i64;
            if matched > 0 {
                if matched > best_match {
                    best_match = matched;
                    allowance = line.allowance;
                } else if matched == best_match && !allowance {
                    allowance = line.allowance;
                }
            }
        }
        allowance
    }
}

fn merge_entries(e1: &Entry, e2: &Entry) -> Entry {
    let e2_agents: std::collections::HashSet<&str> =
        e2.useragents.iter().map(String::as_str).collect();
    Entry {
        useragents: e1
            .useragents
            .iter()
            .filter(|agent| e2_agents.contains(agent.as_str()))
            .cloned()
            .collect(),
        rulelines: [e1.rulelines.clone(), e2.rulelines.clone()].concat(),
    }
}

fn collapse_stars(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut stars = 0;
    for c in path.chars() {
        if c == '*' {
            stars += 1;
            if stars == 1 {
                out.push('*');
            }
        } else {
            stars = 0;
            out.push(c);
        }
    }
    out
}

fn collapse_dollars(path: &str) -> String {
    // re.sub(r'[$][$*]+', '$', path)
    let bytes = path.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j] == b'$' || bytes[j] == b'*') {
                j += 1;
            }
            out.push(b'$');
            i = j;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The path and query of a URL, without scheme, netloc or fragment. This is
/// what `urlsplit` followed by `urlunsplit(None, None, path, params, query)`
/// produces: empty when there is no path, `?query` when the path is empty,
/// and `/path?query` otherwise.
fn extract_path_query(url: &str) -> String {
    let without_fragment = url.split('#').next().unwrap_or_default();
    match without_fragment.find("://") {
        Some(scheme_end) => {
            let rest = &without_fragment[scheme_end + 3..];
            let netloc_end = rest.find(['/', '?']).unwrap_or(rest.len());
            rest[netloc_end..].to_string()
        }
        None => without_fragment.to_string(),
    }
}

fn normalize_uri(path: &str) -> String {
    match path.split_once('?') {
        Some((path, query)) => {
            // re.sub(r'[^=&]+', normalize, query): every run of characters that
            // are not '=' or '&' is normalized, and the separators are kept
            let mut normalized = String::new();
            let mut run = String::new();
            for c in query.chars() {
                if c == '=' || c == '&' {
                    if !run.is_empty() {
                        normalized.push_str(&normalize(&run));
                        run.clear();
                    }
                    normalized.push(c);
                } else {
                    run.push(c);
                }
            }
            if !run.is_empty() {
                normalized.push_str(&normalize(&run));
            }
            format!("{}?{normalized}", normalize(path))
        }
        None => normalize(path),
    }
}

fn normalize_pattern(path: &str) -> String {
    match path.split_once('?') {
        Some((path, query)) => {
            let path = normalize_runs(path, &['*', '$']);
            let query = normalize_runs(query, &['=', '&', '*', '$']);
            format!("{path}?{query}")
        }
        None => normalize_runs(path, &['*', '$']),
    }
}

/// Normalize every maximal run of characters not in `keep`.
fn normalize_runs(text: &str, keep: &[char]) -> String {
    let mut out = String::new();
    let mut run = String::new();
    for c in text.chars() {
        if keep.contains(&c) {
            if !run.is_empty() {
                out.push_str(&normalize(&run));
                run.clear();
            }
            out.push(c);
        } else {
            run.push(c);
        }
    }
    if !run.is_empty() {
        out.push_str(&normalize(&run));
    }
    out
}

/// Decode percent-escapes, then re-encode everything but the characters
/// `urllib.parse.quote` never quotes.
fn normalize(path: &str) -> String {
    let decoded = percent_decode(path);
    let mut out = String::new();
    for byte in decoded.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-' | b'~' | b'/') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && bytes[i + 1].is_ascii_hexdigit()
            && bytes[i + 2].is_ascii_hexdigit()
        {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("00");
            out.push(u8::from_str_radix(hex, 16).unwrap_or(b'?'));
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn find_from(haystack: &str, needle: &str, from: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(from.min(haystack.len()));
    }
    haystack[from..].find(needle).map(|at| at + from)
}

fn rfind_from(haystack: &str, needle: &str, from: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(haystack.len());
    }
    haystack[from..].rfind(needle).map(|at| at + from)
}

#[cfg(test)]
mod tests;
