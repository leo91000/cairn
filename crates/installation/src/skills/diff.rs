//! Line diffs for previewing a skill source update.
use cairn_contracts::SkillDiffLine;

/// Files longer than this are reported as changed without lines.
pub const MAX_LINES: usize = 2000;
const CONTEXT: usize = 3;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Context,
    Added,
    Removed,
}

/// Unified line diff from `old` to `new`, keeping three lines of context
/// around changes. Longer unchanged runs become one `gap` line.
pub fn lines(old: &str, new: &str) -> Option<Vec<SkillDiffLine>> {
    let old = old.lines().collect::<Vec<_>>();
    let new = new.lines().collect::<Vec<_>>();
    if old.len() > MAX_LINES || new.len() > MAX_LINES {
        return None;
    }

    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let (a, b) = (
        &old[prefix..old.len() - suffix],
        &new[prefix..new.len() - suffix],
    );

    // Longest common subsequence over the changed middle only.
    let width = b.len() + 1;
    let mut table = vec![0u16; (a.len() + 1) * width];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            table[i * width + j] = if a[i] == b[j] {
                table[(i + 1) * width + j + 1] + 1
            } else {
                table[(i + 1) * width + j].max(table[i * width + j + 1])
            };
        }
    }

    let mut edits = old[..prefix]
        .iter()
        .map(|line| (Kind::Context, *line))
        .collect::<Vec<_>>();
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            edits.push((Kind::Context, a[i]));
            i += 1;
            j += 1;
        } else if i < a.len()
            && (j == b.len() || table[(i + 1) * width + j] >= table[i * width + j + 1])
        {
            // Removals come first, as in unified diffs.
            edits.push((Kind::Removed, a[i]));
            i += 1;
        } else {
            edits.push((Kind::Added, b[j]));
            j += 1;
        }
    }
    edits.extend(
        old[old.len() - suffix..]
            .iter()
            .map(|line| (Kind::Context, *line)),
    );

    Some(collapse(&edits))
}

fn collapse(edits: &[(Kind, &str)]) -> Vec<SkillDiffLine> {
    let changed = edits
        .iter()
        .enumerate()
        .filter(|(_, (kind, _))| *kind != Kind::Context)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let near = |index: usize| {
        changed
            .iter()
            .any(|&change| index + CONTEXT >= change && index <= change + CONTEXT)
    };

    let mut result = Vec::new();
    let mut skipped = 0;
    for (index, (kind, text)) in edits.iter().enumerate() {
        if *kind == Kind::Context && !near(index) {
            skipped += 1;
            continue;
        }
        if skipped > 0 {
            result.push(gap(skipped));
            skipped = 0;
        }
        let kind = match kind {
            Kind::Context => "context",
            Kind::Added => "added",
            Kind::Removed => "removed",
        };
        result.push(SkillDiffLine {
            kind: kind.to_owned(),
            text: (*text).to_owned(),
        });
    }
    if skipped > 0 {
        result.push(gap(skipped));
    }
    result
}

fn gap(count: usize) -> SkillDiffLine {
    let unit = if count == 1 { "line" } else { "lines" };
    SkillDiffLine {
        kind: "gap".to_owned(),
        text: format!("{count} unchanged {unit}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(lines: &[SkillDiffLine]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                let mark = match line.kind.as_str() {
                    "added" => "+",
                    "removed" => "-",
                    "gap" => "~",
                    _ => " ",
                };
                format!("{mark}{}", line.text)
            })
            .collect()
    }

    #[test]
    fn keeps_context_around_changes_and_collapses_the_rest() {
        let old = (1..=20).map(|n| n.to_string()).collect::<Vec<_>>();
        let mut new = old.clone();
        new[9] = "ten".into();
        new.insert(15, "extra".into());
        let lines = lines(&old.join("\n"), &new.join("\n")).unwrap();
        assert_eq!(
            render(&lines),
            [
                "~6 unchanged lines",
                " 7",
                " 8",
                " 9",
                "-10",
                "+ten",
                " 11",
                " 12",
                " 13",
                " 14",
                " 15",
                "+extra",
                " 16",
                " 17",
                " 18",
                "~2 unchanged lines",
            ]
        );
    }

    #[test]
    fn reports_whole_file_additions_and_skips_huge_files() {
        assert_eq!(render(&lines("", "a\nb").unwrap()), ["+a", "+b"]);
        assert_eq!(render(&lines("a", "").unwrap()), ["-a"]);
        assert!(lines(&"x\n".repeat(MAX_LINES + 1), "").is_none());
    }
}
