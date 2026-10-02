use std::collections::{BTreeMap, BTreeSet};

use crate::config::CodeSnippet;

/// Search an already-filtered catalog; never reintroduce excluded snippets.
pub fn search_snippets<'a>(
    snippets: &'a BTreeMap<String, CodeSnippet>,
    query: Option<&str>,
    min_hits: usize,
) -> Vec<(&'a String, &'a CodeSnippet)> {
    let query = query.unwrap_or_default().to_lowercase();
    let terms: BTreeSet<_> = query
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .collect();
    if terms.is_empty() {
        return snippets.iter().collect();
    }

    let mut ranked: Vec<_> = snippets
        .iter()
        .map(|(id, snippet)| {
            let title = snippet.title.to_lowercase();
            let description = snippet.description.to_lowercase();
            let mut matches = 0;
            let mut title_matches = 0;
            for term in &terms {
                if title.contains(term) {
                    matches += 1;
                    title_matches += 1;
                } else if description.contains(term) {
                    matches += 1;
                }
            }
            ((matches, title_matches), id, snippet)
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    let matching_count = ranked.iter().take_while(|entry| entry.0.0 > 0).count();
    ranked
        .into_iter()
        .take(matching_count.max(min_hits))
        .map(|(_, id, snippet)| (id, snippet))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> BTreeMap<String, CodeSnippet> {
        [
            (
                "xlsx",
                "Create a formatted Excel workbook",
                "Creates a multi-sheet XLSX using XlsxWriter.",
            ),
            (
                "docx",
                "Text document conversion",
                "Converts DOCX, DOC, TXT, and Markdown inputs into DOCX, PDF, TXT, or Markdown.",
            ),
            (
                "pdf",
                "Split PDF into individual pages",
                "Splits a PDF into separate single-page files.",
            ),
            (
                "image",
                "Split image into four equal parts as ZIP",
                "Splits an image into quadrants and packages them in a ZIP.",
            ),
            (
                "quality",
                "Profile CSV data quality with Polars",
                "Profiles missing values, duplicates, and invalid amounts.",
            ),
            (
                "group",
                "Group, aggregate, and rank CSV KPIs with Polars",
                "Calculates grouped sums, counts, ratios, and rankings.",
            ),
            (
                "plan",
                "Join CSVs and compare actuals against plan with Polars",
                "Reconciles plan and actual CSVs with a full join.",
            ),
            (
                "time",
                "Summarize CSV time series and export Parquet with Polars",
                "Produces daily summaries and cleaned Parquet data.",
            ),
            (
                "forecast",
                "Calculate probability-weighted CSV forecasts with Polars",
                "Validates amounts, dates, and probabilities before forecasting receipts.",
            ),
        ]
        .into_iter()
        .map(|(id, title, description)| {
            (
                id.to_owned(),
                CodeSnippet {
                    title: title.to_owned(),
                    description: description.to_owned(),
                    tags: vec!["private_tag".to_owned()],
                    required_python_packages: vec![],
                    code_snippet: "private_source".to_owned(),
                },
            )
        })
        .collect()
    }

    fn ids<'a>(
        catalog: &'a BTreeMap<String, CodeSnippet>,
        query: Option<&str>,
        min_hits: usize,
    ) -> Vec<&'a str> {
        search_snippets(catalog, query, min_hits)
            .into_iter()
            .map(|(id, _)| id.as_str())
            .collect()
    }

    #[test]
    fn issue_examples_match_without_fallback() {
        let catalog = catalog();
        assert_eq!(ids(&catalog, Some("DOCX create document"), 0)[0], "docx");
        assert_eq!(ids(&catalog, Some("XLSX risk register"), 0), ["xlsx"]);
        assert_eq!(
            ids(&catalog, Some("  DoCx,\nCREATE-document docx DOCX  "), 0),
            ids(&catalog, Some("DOCX create document"), 0)
        );
    }

    #[test]
    fn empty_queries_list_catalog_in_id_order() {
        let catalog = catalog();
        let expected: Vec<_> = catalog.keys().map(String::as_str).collect();
        for query in [None, Some(""), Some(" \t\n"), Some("?!")] {
            assert_eq!(ids(&catalog, query, 1), expected);
        }
    }

    #[test]
    fn minimum_is_a_floor_capped_by_visible_catalog() {
        let mut catalog = catalog();
        assert!(ids(&catalog, Some("unrelated"), 0).is_empty());
        assert_eq!(ids(&catalog, Some("unrelated"), 2), ["docx", "forecast"]);
        assert_eq!(ids(&catalog, Some("XLSX"), 3), ["xlsx", "docx", "forecast"]);
        assert_eq!(ids(&catalog, Some("Polars"), 1).len(), 5);
        assert_eq!(ids(&catalog, Some("XLSX"), usize::MAX).len(), catalog.len());
        catalog.remove("xlsx");
        assert!(!ids(&catalog, Some("XLSX"), usize::MAX).contains(&"xlsx"));
        assert!(ids(&BTreeMap::new(), Some("XLSX"), 3).is_empty());
    }

    #[test]
    fn ranking_prefers_coverage_then_titles_then_id() {
        let mut catalog = catalog();
        for (id, title, description) in [
            ("a", "other", "needle thread"),
            ("b", "needle", "thread"),
            ("c", "needle", "thread"),
            ("d", "needle", "other"),
        ] {
            let mut snippet = catalog["docx"].clone();
            snippet.title = title.to_owned();
            snippet.description = description.to_owned();
            catalog.insert(id.to_owned(), snippet);
        }
        assert_eq!(
            ids(&catalog, Some("needle thread"), 0),
            ["b", "c", "a", "d"]
        );
        for query in ["private_tag", "private_source"] {
            assert!(ids(&catalog, Some(query), 0).is_empty());
        }
    }

    #[test]
    fn handles_unicode_case_and_substrings() {
        let mut catalog = catalog();
        catalog.get_mut("docx").unwrap().title = "Übersicht Dokumente".to_owned();
        assert_eq!(ids(&catalog, Some("ÜBERSICHT dokument"), 0), ["docx"]);
    }
}
