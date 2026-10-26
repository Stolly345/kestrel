//! Query-text helpers for the search UI: filter chips, match highlighting, autocomplete.
//! The text in the search box stays the single source of truth; chips just edit tokens in it.

/// Whitespace-separated tokens, keeping `"quoted parts"` together (quotes kept).
pub fn tokens(q: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in q.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    Type,
    Date,
    Size,
}

/// (token, menu label) per chip group. "" = Any.
pub fn options(g: Group) -> &'static [(&'static str, &'static str)] {
    match g {
        Group::Type => &[
            ("", "Any type"),
            ("folder:", "Folders"),
            ("file:", "Files"),
            ("doc:", "Documents"),
            ("pic:", "Pictures"),
            ("audio:", "Audio"),
            ("video:", "Video"),
            ("zip:", "Archives"),
            ("exe:", "Programs"),
            ("diagram:", "Diagrams"),
            ("cad:", "CAD and 3D"),
            ("gis:", "Maps and GPS"),
            ("data:", "Data and config"),
            ("cert:", "Keys and certificates"),
            ("lib:", "Libraries and bytecode"),
            ("code:", "Source code"),
            ("font:", "Fonts"),
        ],
        Group::Date => &[
            ("", "Any time"),
            ("dm:today", "Today"),
            ("dm:yesterday", "Yesterday"),
            ("dm:last7days", "Last 7 days"),
            ("dm:last30days", "Last 30 days"),
            ("dm:thisyear", "This year"),
        ],
        Group::Size => &[
            ("", "Any size"),
            ("size:empty", "Empty"),
            ("size:tiny", "Under 10 KB"),
            ("size:<1mb", "Under 1 MB"),
            ("size:>10mb", "Over 10 MB"),
            ("size:>100mb", "Over 100 MB"),
            ("size:>1gb", "Over 1 GB"),
        ],
    }
}

fn in_group(tok: &str, g: Group) -> bool {
    let t = tok.to_ascii_lowercase();
    match g {
        Group::Type => options(Group::Type)[1..].iter().any(|(o, _)| t == *o),
        Group::Date => t.starts_with("dm:") || t.starts_with("datemodified:"),
        Group::Size => t.starts_with("size:"),
    }
}

/// The group's token in the query, if any (the last one wins, like the parser's AND would narrow).
pub fn current(q: &str, g: Group) -> Option<String> {
    tokens(q).into_iter().rev().find(|t| in_group(t, g))
}

/// Short chip label: the option's name, the raw token for custom values, or the group name.
pub fn chip_label(q: &str, g: Group) -> (String, bool) {
    match current(q, g) {
        None => (
            match g {
                Group::Type => "Type",
                Group::Date => "Date",
                Group::Size => "Size",
            }
            .into(),
            false,
        ),
        Some(t) => (
            options(g)
                .iter()
                .find(|(o, _)| o.eq_ignore_ascii_case(&t))
                .map_or(t.clone(), |(_, l)| l.to_string()),
            true,
        ),
    }
}

/// Replace the group's token(s) with `value` ("" removes them).
pub fn set_group(q: &str, g: Group, value: &str) -> String {
    let mut toks: Vec<String> = tokens(q).into_iter().filter(|t| !in_group(t, g)).collect();
    if !value.is_empty() {
        toks.push(value.to_string());
    }
    toks.join(" ")
}

/// Plain words of the query to highlight in names (no modifiers, negations or wildcards).
pub fn highlight_terms(q: &str) -> Vec<String> {
    tokens(q)
        .into_iter()
        .flat_map(|t| t.split('|').map(String::from).collect::<Vec<_>>())
        .map(|t| t.trim_matches('"').to_string())
        .filter(|t| !t.is_empty() && !t.starts_with('!') && !t.contains(':') && !t.contains('/'))
        .map(|t| t.replace(['*', '?'], " "))
        .flat_map(|t| t.split_whitespace().map(|w| w.to_ascii_lowercase()).collect::<Vec<_>>())
        .collect()
}

/// Pango markup for `name` with every case-insensitive occurrence of `terms` in bold.
pub fn highlight_markup(name: &str, terms: &[String]) -> String {
    let lower = name.to_ascii_lowercase(); // same byte offsets as `name`
    let mut marks = vec![false; name.len()];
    for t in terms.iter().filter(|t| !t.is_empty()) {
        let mut from = 0;
        while let Some(p) = lower[from..].find(t.as_str()) {
            let s = from + p;
            marks[s..s + t.len()].iter_mut().for_each(|m| *m = true);
            // step one character, so overlapping matches ("aa" in "aaa") all get marked
            from = s + t.chars().next().map_or(1, char::len_utf8);
        }
    }
    let mut out = String::new();
    let mut i = 0;
    while i < name.len() {
        let on = marks[i];
        let mut j = i;
        while j < name.len() && marks[j] == on {
            j += 1;
        }
        // ascii-lowercased needles only match on char boundaries, so this slice is valid UTF-8
        let part = gtk::glib::markup_escape_text(&name[i..j]);
        if on {
            out += &format!("<b>{part}</b>");
        } else {
            out += &part;
        }
        i = j;
    }
    out
}

pub struct Suggestion {
    /// Replaces the last token of the query.
    pub insert: String,
    pub label: String,
    pub detail: &'static str,
}

const MODIFIERS: &[(&str, &str)] = &[
    ("ext:", "file extension, e.g. ext:pdf;docx"),
    ("size:", "size, e.g. size:>1gb or size:large"),
    ("dm:", "date modified, e.g. dm:today or dm:2026-10"),
    ("folder:", "folders only"),
    ("file:", "files only"),
    ("path:", "match the full path"),
    ("under:", "anything below a folder"),
    ("parent:", "directly inside a folder"),
    ("tag:", "color tag, e.g. tag:red"),
    ("content:", "text inside files (slow)"),
    ("regex:", "regular expression on the name"),
    ("case:", "case-sensitive"),
    ("dupe:", "duplicate names"),
    ("sizedupe:", "duplicate sizes"),
    ("depth:", "folder depth, e.g. depth:<3"),
    ("empty:", "empty folders"),
    ("doc:", "documents"),
    ("pic:", "pictures"),
    ("audio:", "audio"),
    ("video:", "video"),
    ("zip:", "archives"),
    ("exe:", "programs and installers"),
    ("diagram:", "whiteboards, diagrams, mind maps"),
    ("cad:", "CAD, 3D models, 3D printing"),
    ("gis:", "maps, GPS tracks, charts"),
    ("data:", "config, data files, databases"),
    ("cert:", "keys, certificates, vaults"),
    ("lib:", "libraries, object files, bytecode"),
    ("code:", "source code, scripts, web files"),
    ("font:", "fonts"),
];

const VALUES: &[(&str, &[&str])] = &[
    ("dm:", &["today", "yesterday", "thisweek", "thismonth", "thisyear", "last7days", "last30days"]),
    (
        "size:",
        &["empty", "tiny", "small", "medium", "large", "huge", "gigantic", ">1mb", ">100mb", ">1gb"],
    ),
    ("tag:", &["red", "orange", "yellow", "green", "blue", "purple", "gray"]),
    ("ext:", &["pdf", "docx", "xlsx", "jpg", "png", "mp4", "mp3", "zip", "rs", "py", "md", "txt"]),
];

/// Completions for the token being typed at the end of the query.
pub fn complete(q: &str) -> Vec<Suggestion> {
    if q.is_empty() || q.ends_with(char::is_whitespace) {
        return vec![];
    }
    let last = tokens(q).pop().unwrap_or_default();
    let neg = if last.starts_with('!') { "!" } else { "" };
    let word = last.trim_start_matches('!').to_ascii_lowercase();
    if let Some((m, rest)) = word.split_once(':') {
        let m = format!("{m}:");
        let Some((_, vals)) = VALUES.iter().find(|(k, _)| *k == m) else {
            return vec![];
        };
        // after `;` (ext:pdf;do) complete the last list item
        let (done, part) = rest.rsplit_once(';').map_or(("", rest), |(a, b)| (a, b));
        let prefix = if done.is_empty() { m.clone() } else { format!("{m}{done};") };
        return vals
            .iter()
            .filter(|v| v.starts_with(part) && **v != part)
            .map(|v| Suggestion {
                insert: format!("{neg}{prefix}{v}"),
                label: format!("{prefix}{v}"),
                detail: "",
            })
            .collect();
    }
    if word.len() < 2 {
        return vec![];
    }
    MODIFIERS
        .iter()
        .filter(|(m, _)| m.starts_with(&word))
        .map(|(m, d)| Suggestion {
            insert: format!("{neg}{m}"),
            label: m.to_string(),
            detail: d,
        })
        .collect()
}

/// `q` with its last token replaced by `insert`.
pub fn apply(q: &str, insert: &str) -> String {
    let mut t = tokens(q);
    t.pop();
    t.push(insert.to_string());
    t.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chips() {
        let q = "invoice dm:today \"my file\"";
        assert_eq!(tokens(q), ["invoice", "dm:today", "\"my file\""]);
        assert_eq!(current(q, Group::Date).as_deref(), Some("dm:today"));
        assert_eq!(chip_label(q, Group::Date), ("Today".into(), true));
        assert_eq!(chip_label(q, Group::Size), ("Size".into(), false));
        assert_eq!(set_group(q, Group::Date, "dm:thisyear"), "invoice \"my file\" dm:thisyear");
        assert_eq!(set_group(q, Group::Date, ""), "invoice \"my file\"");
        assert_eq!(set_group("x", Group::Type, "pic:"), "x pic:");
        assert_eq!(set_group("x pic:", Group::Type, "doc:"), "x doc:");
        assert_eq!(chip_label("x dm:2026-10", Group::Date), ("dm:2026-10".into(), true), "custom value shows raw");
        assert_eq!(current("ext:pdf", Group::Type), None, "ext: isn't a type chip");
    }

    #[test]
    fn highlight() {
        let t = highlight_terms("Inv ext:pdf !draft \"my file\" a|b path:/x *.rs");
        assert_eq!(t, ["inv", "my", "file", "a", "b", ".rs"]);
        assert_eq!(highlight_markup("Invoice.pdf", &["inv".into()]), "<b>Inv</b>oice.pdf");
        assert_eq!(highlight_markup("a<b>&", &["b".into()]), "a&lt;<b>b</b>&gt;&amp;");
        assert_eq!(highlight_markup("café inv", &["inv".into()]), "café <b>inv</b>", "non-ASCII before a match");
        assert_eq!(highlight_markup("aaa", &["aa".into()]), "<b>aaa</b>", "overlaps merge");
        assert_eq!(highlight_markup("x", &[]), "x");
    }

    #[test]
    fn completion() {
        let s = complete("report ex");
        assert_eq!(s.iter().map(|s| s.insert.as_str()).collect::<Vec<_>>(), ["ext:", "exe:"]);
        assert_eq!(apply("report ex", "ext:"), "report ext:");
        let s = complete("dm:to");
        assert_eq!(s[0].insert, "dm:today");
        assert!(complete("dm:today").is_empty(), "nothing to add to a complete value");
        assert_eq!(complete("!size:hu")[0].insert, "!size:huge");
        assert_eq!(complete("ext:pdf;do")[0].insert, "ext:pdf;docx");
        assert!(complete("e").is_empty(), "one letter is too noisy");
        assert!(complete("report ").is_empty());
        assert!(complete("zzz:").is_empty());
    }
}
