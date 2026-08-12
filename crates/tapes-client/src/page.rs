//! One pagination convention.
//!
//! # The convention
//!
//! Every paginated tapes listing answers with `items` and `next_cursor`, and
//! takes the cursor back under the query name `cursor`. A page is the last one
//! when `next_cursor` is absent, `null`, or empty — three spellings of the same
//! fact, which is exactly why reading it belongs in one place. A client that
//! treated `""` as a cursor would ask for a fourth page forever.
//!
//! # Why this is a floor and not a helper on one surface
//!
//! Paging was previously each consumer's own loop, and the loops differed: one
//! checked `next_cursor` for null and refused to page at all, one read it as a
//! string. Neither carried a stop condition for a server that repeats a cursor.
//! Both surfaces page the same way, so the convention is written once here and
//! [`walk`] is the only loop.

use serde::Deserialize;
use serde_json::Value;

use crate::error::Result;
use crate::transport::WireRequest;

/// The query parameter a page cursor travels under.
pub const CURSOR_PARAM: &str = "cursor";

/// The query parameter a page size travels under.
pub const LIMIT_PARAM: &str = "limit";

/// One page of a listing.
#[derive(Debug, Clone, Deserialize)]
pub struct Page<T> {
    /// This page's items.
    #[serde(default = "Vec::new")]
    pub items: Vec<T>,
    /// The cursor for the next page, when there is one.
    #[serde(default)]
    pub next_cursor: Option<String>,
}

impl<T> Page<T> {
    /// The cursor to ask for the next page with, or `None` at the end.
    ///
    /// Empty is end-of-listing, not a cursor: a server that renders "no more
    /// pages" as `""` rather than `null` must not send a client round again.
    #[must_use]
    pub fn next(&self) -> Option<&str> {
        self.next_cursor
            .as_deref()
            .filter(|cursor| !cursor.is_empty())
    }
}

impl<T> Default for Page<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            next_cursor: None,
        }
    }
}

/// Whether a raw listing document says there are more pages.
///
/// For callers holding an undecoded document — the fidelity operations keep
/// their responses as [`Value`] — so that "is this listing complete?" is read
/// the same way whether or not the items were modelled.
#[must_use]
pub fn more_pages(document: &Value) -> bool {
    document
        .get("next_cursor")
        .and_then(Value::as_str)
        .is_some_and(|cursor| !cursor.is_empty())
}

/// Set a page cursor on a request, replacing any cursor already on it.
pub fn set_cursor(request: &mut WireRequest<'_>, cursor: Option<&str>) {
    request.query.retain(|(name, _)| name != CURSOR_PARAM);
    if let Some(cursor) = cursor {
        request
            .query
            .push((CURSOR_PARAM.to_owned(), cursor.to_owned()));
    }
}

/// Follow `next_cursor` to the end of a listing, collecting every item.
///
/// `fetch` is called once per page with the cursor for that page — `None` for
/// the first. The walk stops when a page reports no next cursor, and also if a
/// server repeats a cursor it already served: that is a server bug, but an
/// unbounded loop in a CLI reads as a hang, and a hang is the hardest failure
/// to attribute.
pub async fn walk<T, F, Fut>(mut fetch: F) -> Result<Vec<T>>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: Future<Output = Result<Page<T>>>,
{
    let mut items = Vec::new();
    let mut cursor: Option<String> = None;
    let mut seen: Vec<String> = Vec::new();

    loop {
        let mut page = fetch(cursor.clone()).await?;
        items.append(&mut page.items);
        match page.next() {
            Some(next) if !seen.iter().any(|prior| prior == next) => {
                seen.push(next.to_owned());
                cursor = Some(next.to_owned());
            }
            _ => return Ok(items),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Decode one canned page body.
    fn parse(body: &str) -> Page<i32> {
        serde_json::from_str(body).unwrap()
    }

    #[test]
    fn an_empty_cursor_is_the_end_of_the_listing_not_a_fourth_page() {
        // The three spellings of "no more pages", read one way.
        for raw in [
            r#"{"items":[]}"#,
            r#"{"items":[],"next_cursor":null}"#,
            r#"{"items":[],"next_cursor":""}"#,
        ] {
            let page: Page<Value> = serde_json::from_str(raw).unwrap();
            assert_eq!(page.next(), None, "{raw}");
        }
        let page: Page<Value> = serde_json::from_str(r#"{"items":[],"next_cursor":"c1"}"#).unwrap();
        assert_eq!(page.next(), Some("c1"));
    }

    #[test]
    fn a_raw_document_reads_the_same_way_as_a_decoded_page() {
        // The fidelity operations never decode their items; "is there more?"
        // must not depend on whether they did.
        assert!(!more_pages(&serde_json::json!({"items": []})));
        assert!(!more_pages(
            &serde_json::json!({"items": [], "next_cursor": null})
        ));
        assert!(!more_pages(
            &serde_json::json!({"items": [], "next_cursor": ""})
        ));
        assert!(more_pages(
            &serde_json::json!({"items": [], "next_cursor": "c1"})
        ));
    }

    #[tokio::test]
    async fn a_walk_follows_cursors_to_the_end() {
        let pages = [
            (None, r#"{"items":[1,2],"next_cursor":"c1"}"#),
            (Some("c1"), r#"{"items":[3],"next_cursor":"c2"}"#),
            (Some("c2"), r#"{"items":[4],"next_cursor":null}"#),
        ];
        let asked: RefCell<Vec<Option<String>>> = RefCell::new(Vec::new());

        let fetch = |cursor: Option<String>| {
            asked.borrow_mut().push(cursor.clone());
            let page = pages
                .iter()
                .find(|(want, _)| want.map(ToOwned::to_owned) == cursor)
                .map(|(_, body)| parse(body))
                .unwrap();
            std::future::ready(Ok(page))
        };
        let items: Vec<i32> = walk(fetch).await.unwrap();

        assert_eq!(items, vec![1, 2, 3, 4]);
        assert_eq!(
            asked.into_inner(),
            vec![None, Some("c1".to_owned()), Some("c2".to_owned())],
        );
    }

    #[tokio::test]
    async fn a_repeated_cursor_stops_the_walk_rather_than_hanging() {
        // A server that keeps handing back the same cursor is a server bug,
        // but an unbounded loop in a CLI reads as a hang — the hardest kind of
        // failure to attribute to its cause.
        let calls = RefCell::new(0_usize);
        let fetch = |_| {
            *calls.borrow_mut() += 1;
            std::future::ready(Ok(parse(r#"{"items":[1],"next_cursor":"stuck"}"#)))
        };
        let items: Vec<i32> = walk(fetch).await.unwrap();

        assert_eq!(*calls.borrow(), 2, "the repeat must end the walk");
        assert_eq!(items, vec![1, 1]);
    }

    #[test]
    fn setting_a_cursor_replaces_rather_than_appends() {
        // Two `cursor` parameters on one URL is a request whose meaning is the
        // server's to guess.
        let mut request = WireRequest {
            method: "GET",
            path: "/v1/sessions",
            query: vec![
                ("limit".to_owned(), "25".to_owned()),
                (CURSOR_PARAM.to_owned(), "old".to_owned()),
            ],
            ..Default::default()
        };
        set_cursor(&mut request, Some("new"));
        assert_eq!(
            request.query,
            vec![
                ("limit".to_owned(), "25".to_owned()),
                (CURSOR_PARAM.to_owned(), "new".to_owned()),
            ],
        );
        set_cursor(&mut request, None);
        assert_eq!(request.query, vec![("limit".to_owned(), "25".to_owned())]);
    }
}
