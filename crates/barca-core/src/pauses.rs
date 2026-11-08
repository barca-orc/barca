//! Paused nodes (`barca pause`, `POST /pauses`): what a pause covers, and what a run does
//! about it. A scheduled tick does not compute a paused node: a paused target is a skipped
//! step (reason `paused`), and a paused node upstream of a target is read as it is, like a
//! `Manual` one (a target that reads a paused node that has never been computed is skipped
//! too). A request (`barca get`, `barca run`, `POST /runs`) that names a paused node is
//! refused unless it says it means it (`--allow-paused`, `allow_paused`). The pauses
//! themselves are rows of `asset_pauses` (`crate::db::get_asset_pauses`).

use crate::BarcaError;
use crate::dag::Dag;
use crate::targets::{short_name, target_name_matches};
use std::collections::{HashSet, VecDeque};

/// The nodes a pause of `target` covers, in dependency order: the node `target` names (as a
/// run target is named), or every node of the source file `target` names when it ends in
/// `.py`; with `downstream`, everything below them too.
pub fn scope(dag: &Dag, target: &str, downstream: bool) -> Result<Vec<String>, BarcaError> {
    let roots: Vec<String> = if target.ends_with(".py") {
        let file = target.trim_start_matches("./");
        let ids: Vec<String> = dag
            .topo_order()
            .into_iter()
            .filter(|id| {
                id.rsplit_once(':').is_some_and(|(f, _)| {
                    f == file
                        || f.ends_with(&format!("/{file}"))
                        || file.ends_with(&format!("/{f}"))
                })
            })
            .map(str::to_string)
            .collect();
        if ids.is_empty() {
            return Err(BarcaError::Usage(format!(
                "no node is defined in '{target}'\nName a file the pipeline reads (`barca list`)."
            )));
        }
        ids
    } else {
        vec![
            crate::targets::resolve_target(dag, Some(target), "")?
                .expect("a named target resolves"),
        ]
    };
    Ok(if downstream {
        below(dag, &roots)
    } else {
        roots
    })
}

/// `ids` and every node downstream of them, transitively, in dependency order.
pub fn below(dag: &Dag, ids: &[String]) -> Vec<String> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut queue: VecDeque<&str> = VecDeque::new();
    for id in ids {
        if let Some(node) = dag.get_node(id)
            && seen.insert(node.id.as_str())
        {
            queue.push_back(node.id.as_str());
        }
    }
    while let Some(id) = queue.pop_front() {
        for down in dag.downstream(id) {
            if seen.insert(down) {
                queue.push_back(down);
            }
        }
    }
    dag.topo_order()
        .into_iter()
        .filter(|id| seen.contains(id))
        .map(str::to_string)
        .collect()
}

/// The first of `names` (run targets as given) that names a paused node, with that node's id.
/// Matched by name, without the DAG: a name matches a paused id as a run target matches it.
pub fn paused_target<'a>(
    names: &'a [String],
    paused: &[crate::db::AssetPause],
) -> Option<(&'a str, String)> {
    names.iter().flat_map(|n| n.split(',')).find_map(|name| {
        let name = name.trim();
        paused
            .iter()
            .find(|p| !name.is_empty() && target_name_matches(&p.id, name))
            .map(|p| {
                let given = names
                    .iter()
                    .find(|n| n.split(',').any(|part| part.trim() == name))
                    .map(String::as_str)
                    .unwrap_or(name);
                (given, p.id.clone())
            })
    })
}

/// Refuse a request (`barca get`, `barca run`) whose `targets` name a paused node in the
/// metadata DB at `db_path`, unless it was given `--allow-paused` (the caller checks that).
pub async fn check_request(db_path: &str, targets: &[String]) -> Result<(), BarcaError> {
    if targets.is_empty() {
        return Ok(());
    }
    let paused = crate::db::get_asset_pauses(db_path).await?;
    match paused_target(targets, &paused) {
        Some((_, id)) => Err(refused(&id)),
        None => Ok(()),
    }
}

/// The usage error for a request that names the paused node `id`.
pub fn refused(id: &str) -> BarcaError {
    let name = short_name(id);
    BarcaError::Usage(format!(
        "'{name}' is paused: scheduled runs skip it until it is resumed\nRun it anyway with \
         --allow-paused, or resume it first: barca resume {name}"
    ))
}

/// The skipped step's detail for the paused node `id`.
pub(crate) fn detail(id: &str) -> String {
    format!(
        "'{}' is paused; barca resume {} to let scheduled runs compute it",
        short_name(id),
        short_name(id)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dag() -> Dag {
        let src = "from barca import asset\n\
            @asset()\ndef raw():\n    return 1\n\
            @asset(inputs={\"raw\": raw})\ndef clean(raw):\n    return raw\n\
            @asset(inputs={\"clean\": clean})\ndef report(clean):\n    return clean\n\
            @asset()\ndef other():\n    return 2\n";
        Dag::build(&crate::parse::extract_nodes(src, "p.py").unwrap()).expect("valid dag")
    }

    fn names(ids: &[String]) -> Vec<&str> {
        ids.iter().map(|id| short_name(id)).collect()
    }

    #[test]
    fn a_pause_covers_the_node_or_everything_below_it() {
        let d = dag();
        assert_eq!(names(&scope(&d, "clean", false).unwrap()), ["clean"]);
        assert_eq!(
            names(&scope(&d, "clean", true).unwrap()),
            ["clean", "report"]
        );
        let all = scope(&d, "raw", true).unwrap();
        assert_eq!(names(&all), ["raw", "clean", "report"]);
    }

    #[test]
    fn a_file_pause_covers_every_node_of_the_file() {
        let d = dag();
        let ids = scope(&d, "p.py", false).unwrap();
        let mut got = names(&ids);
        got.sort();
        assert_eq!(got, ["clean", "other", "raw", "report"]);
        assert!(scope(&d, "q.py", false).is_err());
        assert!(scope(&d, "nope", false).is_err());
    }

    #[test]
    fn a_request_naming_a_paused_node_is_found_by_name() {
        let paused = vec![crate::db::AssetPause {
            id: "p.py:clean".into(),
            paused_at: 1,
            via: "p.py:clean".into(),
        }];
        let names = vec!["raw,clean".to_string()];
        assert_eq!(
            paused_target(&names, &paused).map(|(_, id)| id),
            Some("p.py:clean".into())
        );
        assert!(paused_target(&["report".to_string()], &paused).is_none());
        assert!(paused_target(&["p.py:clean".to_string()], &paused).is_some());
    }
}
