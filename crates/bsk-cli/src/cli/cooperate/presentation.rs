//! Fold plain numbered fallback controls, retaining business text and named controls.
use serde_json::{Value, json};

fn body(line: &str) -> &str {
    let text = line.trim_start_matches(' ');
    if let Some(rest) = text.strip_prefix("@e")
        && let Some((id, rest)) = rest.split_once(' ')
        && !id.is_empty()
        && id.bytes().all(|b| b.is_ascii_digit())
    {
        return rest;
    }
    text
}
fn numbered(line: &str) -> bool {
    // Exact shape only: state, value and context suffixes are always retained.
    body(line)
        .strip_prefix("button \"item ")
        .and_then(|s| s.strip_suffix('"'))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}
fn protected(line: &str) -> bool {
    matches!(
        body(line).split_whitespace().next(),
        Some("form" | "dialog" | "alertdialog" | "alert" | "status")
    )
}

pub(crate) fn filter(reply: &mut Value, needles: &[String], context: usize) {
    let Some(text) = reply.pointer("/observation/text").and_then(Value::as_str) else {
        return;
    };
    let lines: Vec<&str> = text.lines().collect();
    let capture_truncated = reply
        .pointer("/observation/truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let counts: Vec<usize> = needles
        .iter()
        .map(|n| lines.iter().filter(|l| l.contains(n)).count())
        .collect();
    let matched = lines
        .iter()
        .filter(|l| needles.iter().any(|n| l.contains(n)))
        .count();
    let mut keep = vec![true; lines.len()];
    let mut fallback = None;
    if !text.starts_with("@vom 1\n") || lines.iter().any(|l| l.starts_with('\t')) {
        fallback = Some("unknown_format");
    } else if counts.contains(&0) {
        fallback = Some("no_match");
    } else {
        let mut regions: Vec<usize> = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let indent = line.len() - line.trim_start_matches(' ').len();
            while regions.last().is_some_and(|&depth| depth >= indent) {
                regions.pop();
            }
            if protected(line) {
                regions.push(indent);
            }
            keep[i] = !regions.is_empty() || !numbered(line);
        }
        for (i, line) in lines.iter().enumerate() {
            if needles.iter().any(|n| line.contains(n)) {
                let end = i.saturating_add(context).saturating_add(1).min(lines.len());
                keep[i.saturating_sub(context)..end].fill(true);
            }
        }
        let mut ancestors: Vec<(usize, usize)> = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let indent = line.len() - line.trim_start_matches(' ').len();
            while ancestors.last().is_some_and(|&(_, depth)| depth >= indent) {
                ancestors.pop();
            }
            if keep[i] {
                for &(parent, _) in &ancestors {
                    keep[parent] = true;
                }
            }
            ancestors.push((i, indent));
        }
    }
    let mut output = Vec::new();
    let mut pending = 0;
    for (i, line) in lines.iter().enumerate() {
        if keep[i] {
            if pending > 0 {
                output.push(format!(
                    "… {pending} numbered controls omitted (partial presentation)"
                ));
                pending = 0;
            }
            output.push((*line).to_owned());
        } else {
            pending += 1;
        }
    }
    if pending > 0 {
        output.push(format!(
            "… {pending} numbered controls omitted (partial presentation)"
        ));
    }
    let candidate = output.join("\n");
    let mut omitted = keep.iter().filter(|&&x| !x).count();
    if omitted > 0 && candidate.len() >= text.len() {
        omitted = 0;
        fallback = Some("no_size_benefit");
    }
    let result = if omitted == 0 {
        text.to_owned()
    } else {
        candidate
    };
    let total = lines.len();
    reply["observation"]["text"] = result.into();
    // Legacy consumers must also see that displayed text is incomplete.
    reply["observation"]["capture_truncated"] = capture_truncated.into();
    reply["observation"]["truncated"] = (capture_truncated || omitted > 0).into();
    reply["presentation"] = json!({
        "filtered": omitted > 0, "complete": !capture_truncated && omitted == 0,
        "capture_truncated": capture_truncated, "strategy": "conservative_numbered_controls",
        "match_text": needles, "matches_per_query": counts, "context_lines": context,
        "matched_lines": matched, "total_lines": total, "shown_lines": total - omitted,
        "omitted_lines": omitted, "fallback_reason": fallback,
        "scope": "CLI display only; original refs, ref_count, context and full observation guard unchanged",
        "warning": "Named controls, business text, state hints and complete forms/dialogs are retained. Omitted numbered controls remain unknown; use the full reply before final business decisions."
    });
}

// ── baseline diff presentation ──────────────────────────────────────────────
/** One changed range in the new text: [start, end). */
#[derive(Debug, Clone, Copy)]
enum Op {
    Change(usize, usize),
}

/// Line-level change ranges from `old` to `new`; `None` when the page is too big
/// to diff cheaply (the caller then presents the full text).
fn script(old: &[&str], new: &[&str]) -> Option<Vec<Op>> {
    if old.len().saturating_mul(new.len()) > 4_000_000 {
        return None;
    }
    let n = new.len();
    let m = old.len();
    let width = m + 1;
    let mut table = vec![0u32; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            let at = i * width + j;
            table[at] = if new[i] == old[j] {
                table[(i + 1) * width + j + 1] + 1
            } else {
                table[(i + 1) * width + j].max(table[i * width + j + 1])
            };
        }
    }
    let mut changed = vec![false; n];
    let mut removed = 0usize;
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if new[i] == old[j] {
            i += 1;
            j += 1;
        } else if table[(i + 1) * width + j] >= table[i * width + j + 1] {
            changed[i] = true;
            i += 1;
        } else {
            removed += 1;
            j += 1;
        }
    }
    changed[i..].fill(true);
    // A deletion with no replacement still needs one anchor line so the hunk shows.
    if removed > 0 && !changed.iter().any(|&flag| flag) && n > 0 {
        changed[i.min(n - 1)] = true;
    }
    let mut ops = Vec::new();
    let mut start: Option<usize> = None;
    for (index, is_change) in changed
        .iter()
        .copied()
        .chain(std::iter::once(false))
        .enumerate()
    {
        match (is_change, start) {
            (true, None) => start = Some(index),
            (false, Some(from)) => {
                ops.push(Op::Change(from, index));
                start = None;
            }
            _ => {}
        }
    }
    Some(ops)
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

/// Present a fresh capture as only the lines that changed since `baseline`, with
/// a few unchanged context lines and their ancestors. Falls back to the full text
/// for unknown, truncated or barely-changed views. Returns whether it compacted.
pub(crate) fn diff_against(reply: &mut Value, baseline: &Value, context: usize) -> bool {
    if reply.get("observation").is_none()
        || reply.get("kind").and_then(Value::as_str) != Some("baseline")
    {
        return false;
    }
    // A filtered predecessor is a partial view; diffing against it would drop lines.
    if baseline
        .pointer("/presentation/filtered")
        .and_then(Value::as_bool)
        == Some(true)
    {
        return false;
    }
    let (Some(new_text), Some(old_text)) = (
        reply
            .pointer("/observation/text")
            .and_then(Value::as_str)
            .map(str::to_owned),
        baseline
            .pointer("/observation/text")
            .and_then(Value::as_str)
            .map(str::to_owned),
    ) else {
        return false;
    };
    if new_text == old_text
        || !new_text.starts_with("@vom 1\n")
        || !old_text.starts_with("@vom 1\n")
        || new_text.contains('\t')
        || old_text.contains('\t')
    {
        return false;
    }
    if reply
        .pointer("/observation/truncated")
        .and_then(Value::as_bool)
        == Some(true)
        || baseline
            .pointer("/observation/truncated")
            .and_then(Value::as_bool)
            == Some(true)
    {
        return false;
    }
    let lines: Vec<&str> = new_text.lines().collect();
    let old_lines: Vec<&str> = old_text.lines().collect();
    let Some(changes) = script(&old_lines, &lines) else {
        return false;
    };
    if changes.is_empty() {
        return false;
    }
    let total = lines.len();
    let mut keep = vec![false; total];
    for (index, line) in lines.iter().enumerate() {
        // Top-level lines (@vom/@view/@layers/L1 page) map the frames; keep them.
        keep[index] |= indent_of(line) == 0;
    }
    for Op::Change(start, end) in &changes {
        let from = start.saturating_sub(context);
        let to = (*end).saturating_add(context).min(total);
        keep[from..to].fill(true);
    }
    // Keep ancestors of every kept line so indentation still reads as a tree.
    let mut ancestors: Vec<(usize, usize)> = Vec::new();
    for index in 0..total {
        let indent = indent_of(lines[index]);
        while ancestors.last().is_some_and(|&(_, depth)| depth >= indent) {
            ancestors.pop();
        }
        if keep[index] {
            for &(parent, _) in &ancestors {
                keep[parent] = true;
            }
        }
        ancestors.push((index, indent));
    }
    let mut output: Vec<String> = Vec::with_capacity(total);
    let mut omitted = 0usize;
    let mut omitted_start = 0usize;
    for (index, line) in lines.iter().enumerate() {
        if keep[index] {
            if omitted > 0 {
                output.push(format!(
                    "… {} unchanged line(s) omitted (lines {}-{})",
                    omitted,
                    omitted_start + 1,
                    index
                ));
                omitted = 0;
            }
            output.push((*line).to_owned());
        } else {
            if omitted == 0 {
                omitted_start = index;
            }
            omitted += 1;
        }
    }
    if omitted > 0 {
        output.push(format!(
            "… {} unchanged line(s) omitted (lines {}-{})",
            omitted,
            omitted_start + 1,
            total
        ));
    }
    let candidate = output.join("\n");
    let omitted_total = keep.iter().filter(|&&kept| !kept).count();
    if omitted_total == 0 || candidate.len() * 4 >= new_text.len() * 3 {
        return false;
    }
    let changed_lines: usize = changes
        .iter()
        .map(|Op::Change(start, end)| end - start)
        .sum();
    let hunks: Vec<Value> = changes
        .iter()
        .map(|Op::Change(start, end)| {
            json!({
                "from_line": start.saturating_sub(context) + 1,
                "to_line": (*end).saturating_add(context).min(total),
                "changed_lines": end - start,
            })
        })
        .collect();
    reply["observation"]["text"] = candidate.clone().into();
    let capture_truncated = reply["observation"]["truncated"].clone();
    reply["observation"]["capture_truncated"] = capture_truncated;
    reply["observation"]["truncated"] = true.into();
    let previous = reply
        .get("presentation")
        .filter(|value| value.is_object())
        .cloned();
    let mut presentation = json!({
        "filtered": true,
        "complete": false,
        "capture_truncated": false,
        "strategy": "baseline_diff",
        "baseline_diff": {
            "changed_lines": changed_lines,
            "unchanged_lines_omitted": omitted_total,
            "total_lines": total,
            "shown_chars": candidate.len(),
            "full_chars": new_text.len(),
            "context_lines": context,
            "hunks": hunks,
        },
        "scope": "CLI display only; refs, ref_count, context, observation_id and guards are unchanged",
        "warning": "Unchanged lines are omitted from this view; the agent already holds them from its predecessor observation and --save-full keeps the full reply. Do not treat this text as a complete page model.",
    });
    if let Some(previous) = previous {
        presentation["previous"] = previous;
    }
    reply["presentation"] = presentation;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    fn page() -> Value {
        let mut text = String::from("@vom 1\nL1 page\n  RootWebArea \"Inventory\"\n");
        for row in 0..30 {
            text.push_str(&format!(
                "    @e{} link \"Product {row}\"\n    StaticText \"Capacity 10\"\n    group\n",
                row * 21
            ));
            for col in 0..20 {
                text.push_str(&format!(
                    "      @e{} button \"item {col}\"\n",
                    row * 21 + col + 1
                ));
            }
        }
        json!({"context_id":"ctx","state":{"mode":"ready"},"activity":[{"type":"edit"}],
            "history_complete":false,"observation":{"text":text,"observation_id":"obs","ref_count":630,"truncated":false,"next_cursor":"cursor"}})
    }
    #[test]
    fn retains_distant_alerts_plain_constraints_prices_and_named_controls() {
        let mut v = page();
        let text = v["observation"]["text"].as_str().unwrap().to_owned();
        let important = "    alert \"Cannot order\"\n      StaticText \"Permission required\"\n    StaticText \"Mandatory fee 80\"\n    @e900 button \"Unavailable\"\n    @e901 button \"item 99\" [disabled]\n    @e902 button \"item 100\" [selected:class]\n    status \"Price changed\"";
        v["observation"]["text"] = format!("{text}{important}").into();
        let original = v.clone();
        filter(&mut v, &["Product 1\"".into()], 12);
        let output = v["observation"]["text"].as_str().unwrap();
        for line in important.lines() {
            assert!(output.contains(line), "lost {line}");
        }
        assert!(output.len() < text.len());
        for key in ["context_id", "state", "activity", "history_complete"] {
            assert_eq!(v[key], original[key]);
        }
        for key in ["observation_id", "ref_count", "next_cursor"] {
            assert_eq!(v["observation"][key], original["observation"][key]);
        }
        assert_eq!(v["observation"]["truncated"], true);
        assert_eq!(v["observation"]["capture_truncated"], false);
        assert_eq!(v["presentation"]["complete"], false);
    }
    #[test]
    fn protects_complete_forms_dialogs_and_state_hints() {
        let mut v = page();
        let region = "    form \"Other form\"\n      @e901 button \"item 99\"\n      @e902 textbox \"Name\" [invalid]\n    dialog \"Review\"\n      @e903 button \"item 100\"\n      StaticText \"No refunds\"\nL2 page … occluded by L1";
        v["observation"]["text"] =
            format!("{}{region}", v["observation"]["text"].as_str().unwrap()).into();
        filter(&mut v, &["Product 1\"".into()], 0);
        assert!(v["observation"]["text"].as_str().unwrap().contains(region));
    }
    #[test]
    fn missing_or_case_mismatched_query_returns_full_view() {
        for queries in [
            vec!["missing".into()],
            vec!["product 1".into()],
            vec!["Product 1".into(), "missing".into()],
        ] {
            let mut v = page();
            let original = v["observation"]["text"].clone();
            filter(&mut v, &queries, 12);
            assert_eq!(v["observation"]["text"], original);
            assert_eq!(v["presentation"]["fallback_reason"], "no_match");
            assert_eq!(v["presentation"]["complete"], true);
        }
    }
    #[test]
    fn truncated_capture_is_never_claimed_complete() {
        let mut v = page();
        v["observation"]["truncated"] = true.into();
        filter(&mut v, &["missing".into()], 0);
        assert_eq!(v["observation"]["capture_truncated"], true);
        assert_eq!(v["observation"]["truncated"], true);
        assert_eq!(v["presentation"]["complete"], false);
    }
    #[test]
    fn duplicate_labels_and_unicode_do_not_silently_select_one() {
        let mut v = page();
        v["observation"]["text"] = v["observation"]["text"]
            .as_str()
            .unwrap()
            .replace("Product 1\"", "商品\"")
            .replace("Product 2\"", "商品\"")
            .into();
        filter(&mut v, &["商品".into()], 24);
        assert_eq!(v["presentation"]["matches_per_query"], json!([2]));
        let text = v["observation"]["text"].as_str().unwrap();
        for label in [
            "@e21 link \"商品\"",
            "@e42 link \"商品\"",
            "@e41 button \"item 19\"",
            "@e62 button \"item 19\"",
        ] {
            assert!(text.contains(label));
        }
    }
    #[test]
    fn unknown_formats_and_small_views_never_expand() {
        for text in [
            "unknown format\n@e1 Target",
            "@vom 1\nL1 page\n  @e1 button \"Target\"\n  @e2 button \"item 0\"",
        ] {
            let mut v = json!({"observation":{"text":text,"truncated":false}});
            filter(&mut v, &["Target".into()], 0);
            assert_eq!(v["observation"]["text"], text);
            assert_eq!(v["presentation"]["filtered"], false);
        }
    }
    #[test]
    fn delta_or_ownership_response_is_unchanged() {
        let mut v = json!({"state":{"mode":"human"},"ready":false});
        let original = v.clone();
        filter(&mut v, &["x".into()], 12);
        assert_eq!(v, original);
    }
    fn view(rows: usize, changed: Option<(usize, &str)>) -> Value {
        let mut text = String::from(
            "@vom 1\n@view 1200x800\n@layers 1\nL1 page\n  RootWebArea \"Inventory\"\n",
        );
        for row in 0..rows {
            text.push_str("    group \"Row\"\n      StaticText \"Capacity 10\"\n");
            let label = match changed {
                Some((index, label)) if index == row => label,
                _ => "Save",
            };
            text.push_str(&format!("      @e{} button \"{label}\"\n", row + 1));
        }
        json!({"kind":"baseline","tab_id":7,"context_id":"c1","observation_id":"obs","history_complete":true,
            "observation":{"text":text,"truncated":false,"observation_id":"obs","ref_count":rows}})
    }
    #[test]
    fn baseline_diff_compacts_an_unchanged_page_to_the_changed_line() {
        let old = view(40, None);
        let mut reply = view(40, Some((20, "Print")));
        let observation_id = reply["observation"]["observation_id"].clone();
        assert!(diff_against(&mut reply, &old, 2));
        let text = reply["observation"]["text"].as_str().unwrap();
        assert!(text.contains("@e21 button \"Print\""), "{text}");
        assert!(text.contains("unchanged line(s) omitted"));
        assert!(text.len() < old["observation"]["text"].as_str().unwrap().len() / 2);
        assert_eq!(reply["presentation"]["strategy"], "baseline_diff");
        assert_eq!(reply["presentation"]["baseline_diff"]["changed_lines"], 1);
        assert_eq!(reply["observation"]["observation_id"], observation_id);
        assert_eq!(reply["observation"]["truncated"], true);
        assert_eq!(reply["observation"]["capture_truncated"], false);
        assert_eq!(reply["observation"]["ref_count"], 40);
    }
    #[test]
    fn deep_changes_keep_their_ancestor_lines() {
        let old = view(30, None);
        let mut reply = view(30, Some((25, "Print")));
        assert!(diff_against(&mut reply, &old, 1));
        let text = reply["observation"]["text"].as_str().unwrap();
        for line in [
            "L1 page",
            "  RootWebArea \"Inventory\"",
            "    group \"Row\"",
        ] {
            assert!(text.contains(line), "lost ancestor {line}: {text}");
        }
    }
    #[test]
    fn mostly_new_pages_and_other_reply_kinds_are_left_unchanged() {
        let old = view(40, None);
        let mut reply = view(40, None);
        let text = reply["observation"]["text"].clone();
        for row in 0..40 {
            let patched = reply["observation"]["text"].as_str().unwrap().replace(
                &format!("@e{} button \"Save\"", row + 1),
                &format!("@e{} button \"Different {row}\"", row + 1),
            );
            reply["observation"]["text"] = patched.into();
        }
        assert!(!diff_against(&mut reply, &old, 2));
        assert_ne!(reply["observation"]["text"], text);
        let mut delta = view(40, Some((3, "Print")));
        delta["kind"] = "delta".into();
        let before = delta["observation"]["text"].clone();
        assert!(!diff_against(&mut delta, &old, 2));
        assert_eq!(delta["observation"]["text"], before);
        let mut unknown = view(40, Some((3, "Print")));
        unknown["observation"]["text"] = "plain text".into();
        assert!(!diff_against(&mut unknown, &old, 2));
    }
    #[test]
    fn filtered_predecessor_falls_back_to_the_full_view() {
        let mut old = view(20, None);
        old["presentation"] = json!({"filtered": true, "strategy": "baseline_diff"});
        let mut reply = view(20, Some((2, "Print")));
        let before = reply["observation"]["text"].clone();
        assert!(!diff_against(&mut reply, &old, 3));
        assert_eq!(reply["observation"]["text"], before);
    }
    #[test]
    fn truncated_or_tabbed_views_are_not_diffed() {
        let old = view(20, None);
        let mut truncated = view(20, Some((2, "Print")));
        truncated["observation"]["truncated"] = true.into();
        let before = truncated["observation"]["text"].clone();
        assert!(!diff_against(&mut truncated, &old, 2));
        assert_eq!(truncated["observation"]["text"], before);
        let mut tabbed = view(20, Some((2, "Print")));
        tabbed["observation"]["text"] =
            format!("\t{}", old["observation"]["text"].as_str().unwrap()).into();
        assert!(!diff_against(&mut tabbed, &old, 2));
    }
}
