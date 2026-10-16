//! One JSON shape for list-style output.
//!
//! Every command that prints a list as JSON prints the same document:
//!
//! ```json
//! {"items": [...], "next_cursor": "...", "query": "..."}
//! ```
//!
//! `items` is always present, `next_cursor` only when there is another page,
//! and anything else a command has to say about the list (the query it ran,
//! the `top_k` it asked for) sits beside them as a top-level key. The two
//! command-line clients used to answer this differently — one printed the
//! server's whole document, the other a bare array — and a script written
//! against one broke on the other. The shape lives here so both print it from
//! one type.
//!
//! The paginated listings already speak this shape on the wire (see
//! [`crate::page`]); [`ListEnvelope`] is the same convention made printable,
//! with room for the extra keys a wire page does not carry.

use std::io::Write;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::page::Page;

/// A list, as list-style JSON output prints it.
///
/// Unknown top-level keys survive a decode and re-encode through `extra`, so
/// a consumer reading another tool's output does not drop what it does not
/// model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListEnvelope<T> {
    /// The list.
    #[serde(default = "Vec::new")]
    pub items: Vec<T>,
    /// The cursor for the next page; absent at the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Everything else said about the list, as top-level keys.
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

impl<T> Default for ListEnvelope<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            next_cursor: None,
            extra: Map::new(),
        }
    }
}

impl<T> ListEnvelope<T> {
    /// A complete list: no further page.
    #[must_use]
    pub fn new(items: Vec<T>) -> Self {
        Self {
            items,
            ..Self::default()
        }
    }

    /// Set the next-page cursor. Empty means "no more pages", so it is
    /// dropped rather than printed — the same rule [`Page::next`] reads.
    #[must_use]
    pub fn with_next_cursor(mut self, cursor: Option<String>) -> Self {
        self.next_cursor = cursor.filter(|cursor| !cursor.is_empty());
        self
    }

    /// Add one top-level key beside `items`.
    ///
    /// `items` and `next_cursor` are the envelope's own; an `extra` key of
    /// either name would be printed twice, so one is ignored here.
    #[must_use]
    pub fn with(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        let key = key.into();
        if key != "items" && key != "next_cursor" {
            self.extra.insert(key, value.into());
        }
        self
    }
}

impl<T: Serialize> ListEnvelope<T> {
    /// Write the envelope as pretty-printed JSON plus a trailing newline.
    ///
    /// # Errors
    ///
    /// A serialization or write failure, as an I/O error.
    pub fn write_json<W: Write>(&self, mut out: W) -> std::io::Result<()> {
        serde_json::to_writer_pretty(&mut out, self).map_err(std::io::Error::other)?;
        out.write_all(b"\n")
    }

    /// Print the envelope to standard output as [`ListEnvelope::write_json`]
    /// writes it.
    ///
    /// # Errors
    ///
    /// A serialization or write failure, as an I/O error.
    pub fn print_json(&self) -> std::io::Result<()> {
        self.write_json(std::io::stdout().lock())
    }
}

impl<T> From<Page<T>> for ListEnvelope<T> {
    fn from(page: Page<T>) -> Self {
        Self::new(page.items).with_next_cursor(page.next_cursor)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_complete_list_prints_items_alone() {
        let envelope = ListEnvelope::new(vec![1, 2]);
        assert_eq!(
            serde_json::to_value(&envelope).unwrap(),
            json!({"items": [1, 2]})
        );
    }

    #[test]
    fn an_empty_cursor_is_the_end_and_is_not_printed() {
        let envelope = ListEnvelope::new(vec![1]).with_next_cursor(Some(String::new()));
        assert_eq!(
            serde_json::to_value(&envelope).unwrap(),
            json!({"items": [1]})
        );
        let envelope = ListEnvelope::new(vec![1]).with_next_cursor(Some("p2".to_owned()));
        assert_eq!(
            serde_json::to_value(&envelope).unwrap(),
            json!({"items": [1], "next_cursor": "p2"}),
        );
    }

    #[test]
    fn extra_keys_sit_beside_items_and_cannot_shadow_them() {
        let envelope = ListEnvelope::new(vec!["a"])
            .with("query", "auth")
            .with("top_k", 5)
            .with("items", "ignored");
        assert_eq!(
            serde_json::to_value(&envelope).unwrap(),
            json!({"items": ["a"], "query": "auth", "top_k": 5}),
        );
    }

    #[test]
    fn unknown_keys_survive_a_round_trip() {
        let back: ListEnvelope<Value> =
            serde_json::from_value(json!({"items": [], "from_the_future": true})).unwrap();
        assert_eq!(back.extra["from_the_future"], json!(true));
        assert_eq!(
            serde_json::to_value(&back).unwrap(),
            json!({"items": [], "from_the_future": true}),
        );
    }

    #[test]
    fn a_wire_page_becomes_an_envelope() {
        let page = Page {
            items: vec![1],
            next_cursor: Some("p2".to_owned()),
        };
        let envelope = ListEnvelope::from(page);
        assert_eq!(envelope.next_cursor.as_deref(), Some("p2"));
    }

    #[test]
    fn written_json_ends_with_a_newline() {
        let mut out = Vec::new();
        ListEnvelope::new(vec![1]).write_json(&mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.ends_with("}\n"), "got: {text:?}");
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            json!({"items": [1]})
        );
    }
}
