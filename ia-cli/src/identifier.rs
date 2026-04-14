//! Shared batch identifier collection for CLI commands.
//!
//! Consolidates the logic for collecting identifiers from positional args,
//! `--itemlist` files, `--search` queries, and stdin into a single module
//! used by all batch commands.

use std::collections::HashSet;
use std::io::IsTerminal;
use std::path::Path;

use anyhow::{bail, Context, Result};
use futures::StreamExt;

use ia_core::identifier::{parse_identifier_line, validate_identifier};
use ia_core::search::SearchOpts;
use ia_core::IaClient;

/// Validate that at most one identifier source is active.
///
/// Sources are: positional args, `--itemlist`, `--search`. Stdin is a fallback
/// (only used when all three are absent) and is not counted.
///
/// Returns an error listing the active sources when more than one is provided.
pub fn validate_sources(
    positional: &[String],
    itemlist: Option<&Path>,
    search: Option<&str>,
) -> Result<()> {
    let mut active = Vec::new();
    if positional.iter().any(|s| !s.trim().is_empty()) {
        active.push("positional identifiers");
    }
    if itemlist.is_some() {
        active.push("--itemlist");
    }
    if search.is_some() {
        active.push("--search");
    }
    if active.len() > 1 {
        bail!(
            "identifier sources are mutually exclusive, but got: {}",
            active.join(", ")
        );
    }
    Ok(())
}

/// Validate all collected identifiers.
///
/// Trims whitespace, rejects empty/whitespace-only strings, and runs
/// `validate_identifier()` on each.
pub fn validate_collected_identifiers(ids: &[String]) -> Result<()> {
    for (i, id) in ids.iter().enumerate() {
        let trimmed = id.trim();
        if trimmed.is_empty() {
            bail!("identifier #{} is empty or whitespace-only", i + 1);
        }
        validate_identifier(trimmed)
            .with_context(|| format!("invalid identifier #{}: {trimmed:?}", i + 1))?;
    }
    Ok(())
}

/// Deduplicate identifiers while preserving first-seen order.
pub fn dedup_identifiers(ids: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    ids.into_iter()
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

/// Collect identifiers from exactly one source, validate, and deduplicate.
///
/// Priority:
/// 1. Positional args (trimmed)
/// 2. `--itemlist` file (parsed line-by-line with `parse_identifier_line`)
/// 3. `--search` query (scrape search results)
/// 4. Stdin fallback (when not a terminal, parsed line-by-line)
pub async fn collect_identifiers(
    positional: &[String],
    itemlist: Option<&Path>,
    search: Option<(&str, &SearchOpts)>,
    client: &IaClient,
) -> Result<Vec<String>> {
    validate_sources(positional, itemlist, search.map(|(q, _)| q))?;

    let mut ids = Vec::new();

    if positional.iter().any(|s| !s.trim().is_empty()) {
        ids.extend(
            positional
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        );
    } else if let Some(path) = itemlist {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read itemlist: {}", path.display()))?;
        for line in content.lines() {
            if let Some(id) = parse_identifier_line(line) {
                ids.push(id);
            }
        }
    } else if let Some((query, opts)) = search {
        let mut stream = ia_core::search::scrape(client, query, opts);
        while let Some(result) = stream.next().await {
            let item = result.context("search failed")?;
            ids.push(item.identifier);
        }
    } else if !std::io::stdin().is_terminal() {
        use std::io::BufRead;
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let line = line.context("failed to read from stdin")?;
            if let Some(id) = parse_identifier_line(&line) {
                ids.push(id);
            }
        }
    }

    let ids = dedup_identifiers(ids);
    validate_collected_identifiers(&ids)?;
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ─── validate_sources ─────────────────────────────────────────────

    #[test]
    fn errors_when_search_and_itemlist_both_provided() {
        let result = validate_sources(&[], Some(Path::new("items.txt")), Some("collection:nasa"));
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("mutually exclusive"),
            "expected 'mutually exclusive' in: {msg}"
        );
        assert!(msg.contains("--itemlist"));
        assert!(msg.contains("--search"));
    }

    #[test]
    fn errors_when_all_three_provided() {
        let result = validate_sources(
            &["item1".into()],
            Some(Path::new("items.txt")),
            Some("collection:nasa"),
        );
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("mutually exclusive"),
            "expected 'mutually exclusive' in: {msg}"
        );
        assert!(msg.contains("positional identifiers"));
        assert!(msg.contains("--itemlist"));
        assert!(msg.contains("--search"));
    }

    #[test]
    fn errors_when_positional_and_search_both_provided() {
        let result = validate_sources(&["item1".into()], None, Some("collection:nasa"));
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("positional identifiers"));
        assert!(msg.contains("--search"));
    }

    #[test]
    fn errors_when_positional_and_itemlist_both_provided() {
        let result = validate_sources(&["item1".into()], Some(Path::new("items.txt")), None);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("positional identifiers"));
        assert!(msg.contains("--itemlist"));
    }

    #[test]
    fn ok_when_only_positional() {
        assert!(validate_sources(&["item1".into()], None, None).is_ok());
    }

    #[test]
    fn ok_when_only_search() {
        assert!(validate_sources(&[], None, Some("collection:nasa")).is_ok());
    }

    #[test]
    fn ok_when_only_itemlist() {
        assert!(validate_sources(&[], Some(Path::new("items.txt")), None).is_ok());
    }

    #[test]
    fn ok_when_no_sources_stdin_fallback() {
        assert!(validate_sources(&[], None, None).is_ok());
    }

    // ─── validate_collected_identifiers ───────────────────────────────

    #[test]
    fn rejects_whitespace_only_identifiers() {
        let result = validate_collected_identifiers(&["   ".into()]);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("empty or whitespace-only"), "got: {msg}");
    }

    #[test]
    fn rejects_empty_identifiers() {
        let result = validate_collected_identifiers(&["".into()]);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("empty or whitespace-only"), "got: {msg}");
    }

    #[test]
    fn accepts_valid_identifiers() {
        assert!(validate_collected_identifiers(&[
            "nasa".into(),
            "my-item-123".into(),
            "test.item".into(),
        ])
        .is_ok());
    }

    // ─── dedup_identifiers ────────────────────────────────────────────

    #[test]
    fn deduplicates_preserving_order() {
        let input = vec![
            "bbb".into(),
            "aaa".into(),
            "bbb".into(),
            "ccc".into(),
            "aaa".into(),
        ];
        let result = dedup_identifiers(input);
        assert_eq!(result, vec!["bbb", "aaa", "ccc"]);
    }

    // ─── collect_identifiers (itemlist integration) ───────────────────

    #[tokio::test]
    async fn collect_from_itemlist_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("items.txt");
        std::fs::write(&file, "nasa\n# comment\n\n  my-item-123  \nnasa\n").unwrap();

        // We need a client but won't use it for itemlist path.
        // Use a dummy config that doesn't require real credentials.
        let config = ia_core::IaConfig::default();
        let client = ia_core::IaClient::from_config(config).unwrap();

        let result = collect_identifiers(&[], Some(&file), None, &client)
            .await
            .unwrap();

        // Should parse lines, dedup, and validate
        assert_eq!(result, vec!["nasa", "my-item-123"]);
    }

    #[tokio::test]
    async fn collect_from_positional_trims_and_deduplicates() {
        let config = ia_core::IaConfig::default();
        let client = ia_core::IaClient::from_config(config).unwrap();

        let result = collect_identifiers(
            &[" nasa ".into(), "nasa".into(), "test-item".into()],
            None,
            None,
            &client,
        )
        .await
        .unwrap();

        assert_eq!(result, vec!["nasa", "test-item"]);
    }

    #[tokio::test]
    async fn collect_rejects_conflicting_sources() {
        let config = ia_core::IaConfig::default();
        let client = ia_core::IaClient::from_config(config).unwrap();

        let path = PathBuf::from("items.txt");
        let result = collect_identifiers(&["nasa".into()], Some(&path), None, &client).await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("mutually exclusive"));
    }
}
