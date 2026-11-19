//! Which node a name in a path names: its full id (`pipeline.py:orders`), or what follows the
//! file in one (`orders`). The routes that take a name share the matching here and keep their
//! own words for no match and for several.

/// Whether `name` names the node `id`.
pub(crate) fn names(id: &str, name: &str) -> bool {
    id == name || id.ends_with(&format!(":{name}"))
}

/// The items of `items` that `name` names, in their order; `id` gives an item's node id.
pub(crate) fn matching<'a, T>(
    items: impl IntoIterator<Item = &'a T>,
    id: impl Fn(&T) -> &str,
    name: &str,
) -> Vec<&'a T> {
    items
        .into_iter()
        .filter(|item| names(id(item), name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_a_full_id_or_what_follows_the_file() {
        let ids = ["a.py:orders".to_string(), "sub/b.py:orders".to_string(), "a.py:reorders".to_string()];
        assert_eq!(matching(&ids, |id| id.as_str(), "orders").len(), 2);
        assert_eq!(matching(&ids, |id| id.as_str(), "a.py:orders"), [&ids[0]]);
        assert!(matching(&ids, |id| id.as_str(), "ders").is_empty());
        assert!(names("a.py:orders", "orders") && !names("a.py:orders", "a.py"));
    }
}
