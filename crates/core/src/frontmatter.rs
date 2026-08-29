//! Front-matter surgery for the `id:` field (SPEC §5.2, §6.3): every note carries its ULID in
//! front matter so moves and copies made outside the app resolve without heuristics.
//!
//! Operates on the raw text with line-level edits so nothing else in the note is touched. Only
//! the CommonMark/pandoc form is recognised: `---` on the very first line, closing `---` on its
//! own line.

/// Byte range of the YAML lines (excluding both fences) and the index just past the closing
/// fence line, if the text starts with a front-matter block.
pub fn block(text: &str) -> Option<(std::ops::Range<usize>, usize)> {
    let rest = text.strip_prefix("---")?;
    let rest = rest.strip_prefix("\r\n").or_else(|| rest.strip_prefix('\n'))?;
    let body_start = text.len() - rest.len();
    let mut pos = body_start;
    for line in text[body_start..].split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == "---" || trimmed == "..." {
            return Some((body_start..pos, pos + line.len()));
        }
        pos += line.len();
    }
    None
}

/// The `id:` value in the front matter, if any (quotes stripped).
pub fn id_of(text: &str) -> Option<String> {
    let (range, _) = block(text)?;
    text[range].lines().find_map(id_value)
}

fn id_value(line: &str) -> Option<String> {
    let v = line.strip_prefix("id:")?.trim();
    Some(v.trim_matches(|c| c == '"' || c == '\'').to_owned())
}

/// Make the note carry `id: <id>` exactly once: adds the field (creating a front-matter block
/// when there is none), collapses duplicate identical `id:` lines, and removes a duplicated
/// leading block (what two replicas adding front matter concurrently merge into). Returns the
/// new text, or `None` when nothing needed to change. A *different* existing id is left alone.
pub fn normalize(text: &str, id: &str) -> Option<String> {
    let mut out = dedupe_leading_blocks(text).unwrap_or_else(|| text.to_owned());
    let changed_blocks = out != text;
    let changed_id = match block(&out) {
        None => {
            out = format!("---\nid: {id}\n---\n{out}");
            true
        }
        Some((range, _)) => {
            let body = &out[range.clone()];
            let ids: Vec<String> = body.lines().filter_map(id_value).collect();
            match ids.as_slice() {
                [only] if only == id => false,
                [] => {
                    let mut body = body.to_owned();
                    if !body.is_empty() && !body.ends_with('\n') {
                        body.push('\n');
                    }
                    body.push_str(&format!("id: {id}\n"));
                    out.replace_range(range, &body);
                    true
                }
                _ if ids.iter().all(|x| x == id) => {
                    // Duplicates of the same id: keep the first line only.
                    let mut seen = false;
                    let kept: String = body
                        .split_inclusive('\n')
                        .filter(|l| {
                            let is_id = id_value(l.trim_end_matches(['\n', '\r'])).is_some();
                            if is_id && seen {
                                return false;
                            }
                            if is_id {
                                seen = true;
                            }
                            true
                        })
                        .collect();
                    out.replace_range(range, &kept);
                    true
                }
                _ => false, // carries a different id: not ours to change
            }
        }
    };
    (changed_blocks || changed_id).then_some(out)
}

/// `---\nA\n---\n---\nA\n---\nbody` → `---\nA\n---\nbody` (repeatedly).
fn dedupe_leading_blocks(text: &str) -> Option<String> {
    let (range, end) = block(text)?;
    let first = &text[..end];
    let mut rest = &text[end..];
    let mut removed = false;
    while let Some((r2, e2)) = block(rest) {
        if rest[r2] == text[range.clone()] {
            rest = &rest[e2..];
            removed = true;
        } else {
            break;
        }
    }
    removed.then(|| format!("{first}{rest}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_block_when_missing() {
        assert_eq!(normalize("# T\n\nbody\n", "X").as_deref(), Some("---\nid: X\n---\n# T\n\nbody\n"));
        assert_eq!(normalize("", "X").as_deref(), Some("---\nid: X\n---\n"));
    }

    #[test]
    fn appends_into_existing_block() {
        let t = "---\ntitle: A\ntags: [x]\n---\n\nbody\n";
        assert_eq!(normalize(t, "X").as_deref(), Some("---\ntitle: A\ntags: [x]\nid: X\n---\n\nbody\n"));
        assert_eq!(id_of("---\ntitle: A\nid: \"Q\"\n---\n").as_deref(), Some("Q"));
    }

    #[test]
    fn idempotent_and_respects_foreign_ids() {
        let t = "---\nid: X\n---\nbody\n";
        assert_eq!(normalize(t, "X"), None);
        assert_eq!(normalize("---\nid: OTHER\n---\nbody\n", "X"), None);
        assert_eq!(block("no front matter\n---\n"), None);
        assert_eq!(block("---\nunterminated\n"), None);
    }

    #[test]
    fn collapses_duplicates_from_concurrent_merges() {
        let merged = "---\nid: X\n---\n---\nid: X\n---\n# T\n";
        assert_eq!(normalize(merged, "X").as_deref(), Some("---\nid: X\n---\n# T\n"));
        let dup_line = "---\ntitle: A\nid: X\nid: X\n---\nbody\n";
        assert_eq!(normalize(dup_line, "X").as_deref(), Some("---\ntitle: A\nid: X\n---\nbody\n"));
    }
}
