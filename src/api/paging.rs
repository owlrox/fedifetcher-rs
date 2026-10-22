//! Gathering a paged collection from a server.

use std::future::Future;

use serde_json::Value;

/// Gather up to `limit` entries from a server that pages its collections.
///
/// `fetch` is handed how many entries are still wanted and everything
/// gathered so far, out of which each server's own idea of where we got to is
/// worked out. It returns a page, or None if that page could not be read:
/// failing on the first page means we learned nothing and say so, while
/// failing later only means we stop early with what we already have.
pub async fn in_pages<F, Fut>(mut fetch: F, limit: i64, page_size: i64) -> Option<Vec<Value>>
where
    F: FnMut(i64, &[Value]) -> Fut,
    Fut: Future<Output = Option<Vec<Value>>>,
{
    let mut gathered: Vec<Value> = Vec::new();
    while gathered.len() < limit as usize {
        let wanted = (page_size).min(limit - gathered.len() as i64);
        let page = fetch(wanted, &gathered).await;
        let Some(page) = page else {
            return if gathered.is_empty() { None } else { Some(gathered) };
        };

        // a server with more to give fills the page it was asked for, so this
        // saves a request for every account that has posted less than we want
        let page_len = page.len();
        gathered.extend(page);
        if page_len < wanted as usize {
            break;
        }
    }
    Some(gathered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn gathers_until_limit() {
        let pages = vec![
            vec![json!(1), json!(2)],
            vec![json!(3), json!(4)],
            vec![json!(5)],
        ];
        let mut calls = 0;
        let result = in_pages(
            |_wanted, _gathered| {
                let page = pages[calls].clone();
                calls += 1;
                async move { Some(page) }
            },
            5,
            2,
        )
        .await
        .unwrap();
        assert_eq!(result, vec![json!(1), json!(2), json!(3), json!(4), json!(5)]);
        assert_eq!(calls, 3);
    }

    #[tokio::test]
    async fn stops_when_a_page_comes_back_short() {
        let result = in_pages(
            |_wanted, _gathered| async move { Some(vec![json!(1)]) },
            10,
            40,
        )
        .await
        .unwrap();
        assert_eq!(result, vec![json!(1)]);
    }

    #[tokio::test]
    async fn first_page_failure_means_none() {
        let result = in_pages(|_wanted, _gathered| async move { None }, 10, 40).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn later_failure_keeps_what_we_have() {
        let mut calls = 0;
        let result = in_pages(
            |_wanted, _gathered| {
                calls += 1;
                async move {
                    if calls == 1 {
                        Some(vec![json!(1), json!(2)])
                    } else {
                        None
                    }
                }
            },
            10,
            2,
        )
        .await
        .unwrap();
        assert_eq!(result, vec![json!(1), json!(2)]);
    }
}
