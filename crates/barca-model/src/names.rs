//! Node names as people write them: a node id is `<file>:<name>`, and a command line names a
//! node by the short name or the full id. A leaf: the graph ([`crate::dag`]) and everything that
//! names a node in a message share these, and [`crate::targets`] re-exports them at their old
//! paths.

/// The function name of a node id (`pipeline.py:src` -> `src`, `pipeline.py:p[k=v]` -> `p`).
pub fn short_name(node_id: &str) -> &str {
    let base = node_id.split('[').next().unwrap_or(node_id);
    base.rsplit(':').next().unwrap_or(base)
}

/// Does a target `name` identify node `id`? A name is a function name (`deploy`), a full id
/// (`pipeline.py:deploy`), or a path-suffixed id (`p.py:deploy` for `sub/p.py:deploy`). It
/// matches only at a `:` or `/` boundary, never as the tail of a longer name, so `deploy` does
/// not select `prod_deploy`.
pub fn target_name_matches(id: &str, name: &str) -> bool {
    if id == name {
        return true;
    }
    match id.strip_suffix(name) {
        Some(prefix) => prefix.ends_with(':') || (name.contains(':') && prefix.ends_with('/')),
        None => false,
    }
}

/// Does the asset part of a `--refresh` selector (short name or full id) identify `node_id`?
pub fn refresh_name_matches(node_id: &str, name: &str) -> bool {
    let name = name.split('[').next().unwrap_or(name);
    node_id == name || node_id.ends_with(&format!(":{name}"))
}
