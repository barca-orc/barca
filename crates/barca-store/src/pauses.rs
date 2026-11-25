//! Paused nodes (`barca pause`, `POST /pauses`): what a pause covers, and what a run does
//! about it. A scheduled tick does not compute a paused node: a paused target is a skipped
//! step (reason `paused`), and a paused node upstream of a target is read as it is, like a
//! `Manual` one (a target that reads a paused node that has never been computed is skipped
//! too). A request (`barca get`, `barca run`, `POST /runs`) that names a paused node is
//! refused unless it says it means it (`--allow-paused`, `allow_paused`). The pauses
//! themselves are rows of `asset_pauses` (`barca_db::db::get_asset_pauses`).

use barca_compile::targets::{short_name, target_name_matches};
use barca_model::BarcaError;
use barca_model::dag::Dag;
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
            barca_compile::targets::resolve_target(dag, Some(target), "")?
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
    paused: &[barca_db::db::AssetPause],
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
    let paused = barca_db::db::get_asset_pauses(db_path).await?;
    match paused_target(targets, &paused) {
        Some((_, id)) => Err(refused(&id)),
        None => Ok(()),
    }
}

/// Every paused node of the metadata DB at `db_path`, with when and through what it was paused.
pub async fn list(db_path: &str) -> Result<Vec<barca_db::db::AssetPause>, BarcaError> {
    barca_db::db::get_asset_pauses(db_path).await
}

/// One paused schedule (`barca pause --schedule`, `POST /schedule/{id}/pause`): since when,
/// and until when (RFC 3339 UTC; `None`: until it is resumed).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PausedSchedule {
    pub id: String,
    pub paused_at: String,
    pub until: Option<String>,
}

/// Every schedule paused at `now` (unix seconds) in the metadata DB at `db_path`, by id. A
/// pause whose `until` has passed is over and is left out.
pub async fn paused_schedules(db_path: &str, now: i64) -> Result<Vec<PausedSchedule>, BarcaError> {
    if std::fs::metadata(db_path).map_or(true, |m| m.len() == 0) {
        return Ok(Vec::new());
    }
    let mut paused: Vec<PausedSchedule> = barca_db::db::get_schedule_pauses(db_path)
        .await?
        .into_iter()
        .filter(|(_, pause)| pause.until.is_none_or(|until| until > now))
        .map(|(id, pause)| PausedSchedule {
            id,
            paused_at: barca_model::time::rfc3339(pause.paused_at),
            until: pause.until.map(barca_model::time::rfc3339),
        })
        .collect();
    paused.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(paused)
}

/// When a timed pause ends (`barca pause --schedule NODE --until`, `?until=`), in unix
/// seconds: a duration from `now` (`45s`, `30m`, `2h`, `7d`, `2w`) or an RFC 3339 time
/// (`2026-10-21T06:00:00Z`). A time that is not after `now` is an error.
pub fn parse_until(text: &str, now: chrono::DateTime<chrono::Utc>) -> Result<i64, String> {
    let t = text.trim();
    let duration = t.char_indices().last().and_then(|(i, c)| {
        let scale: i64 = match c.to_ascii_lowercase() {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            'w' => 7 * 86_400,
            _ => return None,
        };
        (i > 0 && t[..i].bytes().all(|b| b.is_ascii_digit()))
            .then(|| t[..i].parse::<i64>().ok()?.checked_mul(scale))
            .flatten()
    });
    let at = match duration {
        Some(seconds) => now.timestamp().saturating_add(seconds),
        None => chrono::DateTime::parse_from_rfc3339(t)
            .map_err(|_| {
                format!(
                    "until '{text}' is neither a duration nor an RFC 3339 time
                     Use a duration from now such as 2h or 7d, or a time such as 2026-10-21T06:00:00Z."
                )
            })?
            .timestamp(),
    };
    if at <= now.timestamp() {
        return Err(format!(
            "until '{text}' is not in the future
             Give a later time, or leave `until` out to pause until resumed."
        ));
    }
    Ok(at)
}

/// Pause `target` (with `downstream`, everything below it too) at `now` (unix seconds),
/// creating the metadata DB when there is none. Answers the ids it covers ([`scope`]). A node
/// the target names takes this pause over from any pause that already holds it, so resuming
/// that other pause with `--downstream` leaves it paused ([`resume_target`]); a node below
/// them keeps the pause it had.
pub async fn pause(
    cfg: &barca_model::config::ResolvedConfig,
    dag: &Dag,
    target: &str,
    downstream: bool,
    now: i64,
) -> Result<Vec<String>, BarcaError> {
    let named = scope(dag, target, false)?;
    let ids = if downstream {
        below(dag, &named)
    } else {
        named.clone()
    };
    barca_db::open::open_metadata(cfg).await?;
    barca_db::db::pause_assets(&cfg.db_path, &ids, &named, target, now).await?;
    Ok(ids)
}

/// Resume every paused node in the metadata DB at `db_path`, whatever paused it. Answers the
/// ids it resumed.
pub async fn resume_all(db_path: &str) -> Result<Vec<String>, BarcaError> {
    let ids = barca_db::db::get_asset_pauses(db_path)
        .await?
        .into_iter()
        .map(|p| p.id)
        .collect();
    barca_db::db::resume_assets(db_path, None).await?;
    Ok(ids)
}

/// Resume `target` in the metadata DB at `db_path`: the nodes it names, whatever paused them,
/// and with `downstream`, each node below them that a pause of `target` holds (its `via`
/// names the same node or file). A node below them that another pause holds stays paused: a
/// deliberate hold under an incident's `--downstream` pause outlives the incident's resume.
/// Answers the ids it resumed, in dependency order.
pub async fn resume_target(
    db_path: &str,
    dag: &Dag,
    target: &str,
    downstream: bool,
) -> Result<Vec<String>, BarcaError> {
    let named = scope(dag, target, false)?;
    let reach = if downstream {
        below(dag, &named)
    } else {
        named.clone()
    };
    let paused = barca_db::db::get_asset_pauses(db_path).await?;
    let ids: Vec<String> = reach
        .into_iter()
        .filter(|id| {
            paused
                .iter()
                .any(|p| &p.id == id && (named.contains(id) || same_target(dag, &p.via, target)))
        })
        .collect();
    barca_db::db::resume_assets(db_path, Some(&ids)).await?;
    Ok(ids)
}

/// Whether the pause targets `a` and `b` name the same thing: the same text, the same source
/// file, or names that resolve to the same node. A name that no longer resolves (its node was
/// removed) matches its own text only.
fn same_target(dag: &Dag, a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    if a.ends_with(".py") || b.ends_with(".py") {
        let (a, b) = (a.trim_start_matches("./"), b.trim_start_matches("./"));
        return a.ends_with(".py")
            && b.ends_with(".py")
            && (a == b || a.ends_with(&format!("/{b}")) || b.ends_with(&format!("/{a}")));
    }
    let node = |name: &str| {
        barca_compile::targets::resolve_target(dag, Some(name), "")
            .ok()
            .flatten()
    };
    node(a).is_some_and(|id| node(b).as_ref() == Some(&id))
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
pub fn detail(id: &str) -> String {
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
        barca_python::link::build(&barca_python::parse::extract_nodes(src, "p.py").unwrap())
            .expect("valid dag")
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
        let paused = vec![barca_db::db::AssetPause {
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

    fn db_cfg(dir: &std::path::Path) -> barca_model::config::ResolvedConfig {
        let mut cfg =
            barca_model::config::resolve_with(None, dir, barca_model::config::Vars::NONE).unwrap();
        std::fs::create_dir_all(dir.join(".barca")).unwrap();
        cfg.db_path = dir
            .join(".barca/metadata.db")
            .to_string_lossy()
            .into_owned();
        cfg
    }

    async fn paused(cfg: &barca_model::config::ResolvedConfig) -> Vec<(String, String)> {
        list(&cfg.db_path)
            .await
            .unwrap()
            .into_iter()
            .map(|p| (short_name(&p.id).to_string(), p.via))
            .collect()
    }

    #[test]
    fn a_pause_target_matches_its_other_spellings() {
        let d = dag();
        assert!(same_target(&d, "clean", "p.py:clean"));
        assert!(same_target(&d, "./p.py", "p.py"));
        assert!(same_target(&d, "gone", "gone"));
        assert!(!same_target(&d, "clean", "raw"));
        assert!(!same_target(&d, "p.py", "clean"));
        assert!(!same_target(&d, "gone", "clean"));
    }

    /// An operator holds `report`, an incident pauses `raw --downstream` over it (in either
    /// order), and the incident's resume leaves the operator's hold in place (ONE-5892).
    #[tokio::test]
    async fn a_downstream_resume_keeps_a_hold_another_pause_set() {
        let d = dag();
        for hold_first in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let cfg = db_cfg(dir.path());
            if hold_first {
                pause(&cfg, &d, "report", false, 1).await.unwrap();
            }
            let covered = pause(&cfg, &d, "p.py:raw", true, 2).await.unwrap();
            assert_eq!(names(&covered), ["raw", "clean", "report"]);
            if !hold_first {
                pause(&cfg, &d, "report", false, 3).await.unwrap();
            }
            let resumed = resume_target(&cfg.db_path, &d, "raw", true).await.unwrap();
            assert_eq!(
                names(&resumed),
                ["raw", "clean"],
                "hold first: {hold_first}"
            );
            assert_eq!(paused(&cfg).await, [("report".into(), "report".into())]);
        }
    }

    /// The nodes a resume names lose their pause whatever set it; below them, only the
    /// resumed target's pause is lifted.
    #[tokio::test]
    async fn a_resume_lifts_the_named_nodes_and_its_own_pause_below_them() {
        let d = dag();
        let dir = tempfile::tempdir().unwrap();
        let cfg = db_cfg(dir.path());
        pause(&cfg, &d, "raw", true, 1).await.unwrap();
        // Without --downstream, only the named node.
        assert_eq!(
            names(
                &resume_target(&cfg.db_path, &d, "clean", false)
                    .await
                    .unwrap()
            ),
            ["clean"]
        );
        // `clean --downstream` names clean (not paused now) and reaches report, which `raw`
        // paused: report stays.
        assert!(
            resume_target(&cfg.db_path, &d, "clean", true)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            paused(&cfg).await,
            [
                ("raw".into(), "raw".into()),
                ("report".into(), "raw".into())
            ]
        );
        // A file names every node of it, so its resume lifts every pause of them.
        pause(&cfg, &d, "other", false, 2).await.unwrap();
        let resumed = resume_target(&cfg.db_path, &d, "./p.py", false)
            .await
            .unwrap();
        assert_eq!(names(&resumed), ["other", "raw", "report"]);
        assert_eq!(
            resume_all(&cfg.db_path).await.unwrap(),
            Vec::<String>::new()
        );
    }
}
