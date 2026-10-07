//! Full-text search over segment text via the `segments_fts` external-content
//! table (see `schema.rs` for how it is kept in sync).

use fc_core::ConversationId;
use rusqlite::params;

use crate::error::Result;
use crate::model::SearchHit;
use crate::Store;

impl Store {
    /// Matches `query` against segment text. `query` is free-form user input,
    /// not an FTS5 query string: every token is quoted as a literal phrase
    /// before reaching SQLite, so operator syntax (`AND`, `OR`, `NEAR`,
    /// unbalanced `"`) in what the user typed can never produce a query
    /// syntax error — at worst it matches nothing.
    pub fn search(&self, query: &str, limit: u32) -> Result<Vec<SearchHit>> {
        let Some(fts_query) = sanitize_fts_query(query) else {
            return Ok(Vec::new());
        };

        let mut stmt = self.conn.prepare(
            "SELECT s.conversation_id, c.title, s.start_ms, s.end_ms,
                    snippet(segments_fts, 0, '\u{2039}', '\u{203a}', '\u{2026}', 10) AS snip
             FROM segments_fts
             JOIN segments s ON s.id = segments_fts.rowid
             JOIN conversations c ON c.id = s.conversation_id
             WHERE segments_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![fts_query, i64::from(limit)], |row| {
            Ok(SearchHit {
                conversation_id: ConversationId(row.get(0)?),
                conversation_title: row.get(1)?,
                start_ms: row.get::<_, i64>(2)? as u64,
                end_ms: row.get::<_, i64>(3)? as u64,
                snippet: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

/// Turns free-form input into a sequence of quoted FTS5 phrases joined with
/// implicit `AND`. Quoting every token, with embedded `"` doubled per SQL
/// string-literal escaping, means the result is always a syntactically valid
/// MATCH argument — it just may not match anything.
fn sanitize_fts_query(input: &str) -> Option<String> {
    let phrases: Vec<String> = input
        .split_whitespace()
        .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
        .collect();

    if phrases.is_empty() {
        None
    } else {
        Some(phrases.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::sanitize_fts_query;

    #[test]
    fn empty_input_yields_no_query() {
        assert_eq!(sanitize_fts_query(""), None);
        assert_eq!(sanitize_fts_query("   "), None);
    }

    #[test]
    fn operators_and_unbalanced_quotes_are_defused() {
        assert_eq!(sanitize_fts_query("AND"), Some("\"AND\"".to_string()));
        assert_eq!(
            sanitize_fts_query("foo \"bar"),
            Some("\"foo\" \"\"\"bar\"".to_string())
        );
    }
}
