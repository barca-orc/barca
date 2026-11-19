//! Which node a reference means. Works on extracted nodes alone, before any graph exists: the
//! graph ([`crate::dag`]) builds its edges with it, groups ([`crate::groups`]) resolve their
//! members with it, and loading ([`crate::load`]) asks it where an import points.

use std::collections::HashMap;

use crate::extracted::ExtractedNode;
use barca_model::dag_error::DagError;

/// Resolves a reference written in one file (`inputs=`, `collect(...)`, `partitions_from(...)`)
/// to the node it means. In order:
///
/// - `asset_ref("file.py:name")`: the node with that id, else `name` in that file (root-relative,
///   then relative to the referencing file), else the one node named `name`.
/// - an imported name (`from m import name`, `m.name`): `name` in the file the import points at
///   (relative imports from the file's package; absolute ones from the file's directory, then
///   the root), else the one node named `name` (a re-export).
/// - a bare name: the function in the same file, else the one node with that name anywhere.
///
/// Several candidates and nothing more specific is an error, never a guess (#202): before,
/// the last file read silently won.
pub(crate) struct Resolver {
    pub(crate) ids: Vec<String>,
    pub(crate) by_id: HashMap<String, usize>,
    by_file: HashMap<(String, String), usize>,
    by_name: HashMap<String, Vec<usize>>,
}

pub(crate) enum Miss {
    NotFound,
    Ambiguous(Vec<String>),
    NotInModule { module: String, file: String },
}

pub(crate) fn norm_path(p: &std::path::Path) -> String {
    barca_model::config::normalize_lexically(p)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Whether `dir` (root-relative; the root is the working directory) is a package: every
/// directory from the root down to it has an `__init__.py`. The root itself is not.
fn in_package(dir: &std::path::Path) -> bool {
    use std::path::Component;
    let mut current = std::path::PathBuf::new();
    let mut any = false;
    for component in dir.components() {
        match component {
            Component::CurDir => continue,
            Component::Normal(part) => current.push(part),
            _ => return false,
        }
        if !current.join("__init__.py").is_file() {
            return false;
        }
        any = true;
    }
    any
}

pub(crate) fn file_dir(file: &str) -> std::path::PathBuf {
    std::path::Path::new(file)
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_default()
}

impl Resolver {
    pub(crate) fn new(nodes: &[ExtractedNode]) -> Self {
        let mut r = Resolver {
            ids: Vec::with_capacity(nodes.len()),
            by_id: HashMap::new(),
            by_file: HashMap::new(),
            by_name: HashMap::new(),
        };
        for (i, n) in nodes.iter().enumerate() {
            let id = n.continuity_key();
            r.by_id.insert(id.clone(), i);
            r.ids.push(id);
            r.by_file.insert(
                (
                    norm_path(std::path::Path::new(&n.source_file)),
                    n.function_name.clone(),
                ),
                i,
            );
            r.by_name
                .entry(n.function_name.clone())
                .or_default()
                .push(i);
        }
        r
    }

    pub(crate) fn resolve_id(
        &self,
        file: &str,
        reference: &barca_model::model::NodeRef,
    ) -> Option<String> {
        self.resolve(file, reference)
            .ok()
            .map(|i| self.ids[i].clone())
    }

    pub(crate) fn in_file(&self, file: &std::path::Path, name: &str) -> Option<usize> {
        self.by_file
            .get(&(norm_path(file), name.to_string()))
            .copied()
    }

    fn unique(&self, name: &str) -> Result<usize, Miss> {
        match self.by_name.get(name).map(Vec::as_slice) {
            Some([one]) => Ok(*one),
            Some(many) if !many.is_empty() => Err(Miss::Ambiguous(
                many.iter().map(|&i| self.ids[i].clone()).collect(),
            )),
            _ => Err(Miss::NotFound),
        }
    }

    /// Candidate files for `module` imported from `from_file`, most specific first.
    pub(crate) fn module_files(from_file: &str, module: &str) -> Vec<std::path::PathBuf> {
        let level = module.chars().take_while(|c| *c == '.').count();
        let rest = module[level..].replace('.', "/");
        let mut bases = Vec::new();
        if level > 0 {
            let mut base = file_dir(from_file);
            for _ in 1..level {
                base = base.join("..");
            }
            bases.push(base);
        } else if in_package(&file_dir(from_file)) {
            // A file inside a package is imported by its dotted name with only the root on
            // the import path (`project_modules::PipelineLayout`), so `from pkg.mod import x`
            // in `pkg/a.py` is `pkg/mod.py`, never `pkg/pkg/mod.py`.
            bases.push(std::path::PathBuf::new());
        } else {
            bases.push(file_dir(from_file));
            bases.push(std::path::PathBuf::new());
        }
        let mut out = Vec::new();
        for base in bases {
            if rest.is_empty() {
                out.push(base.join("__init__.py"));
            } else {
                out.push(base.join(format!("{rest}.py")));
                out.push(base.join(&rest).join("__init__.py"));
            }
        }
        out
    }

    pub(crate) fn resolve(
        &self,
        from_file: &str,
        r: &barca_model::model::NodeRef,
    ) -> Result<usize, Miss> {
        use barca_model::model::NodeRef;
        match r {
            NodeRef::FunctionName(name) => self
                .in_file(std::path::Path::new(from_file), name)
                .map_or_else(|| self.unique(name), Ok),
            NodeRef::Canonical(s) => {
                if let Some(&i) = self.by_id.get(s) {
                    return Ok(i);
                }
                let Some((path, name)) = s.rsplit_once(':') else {
                    return self.unique(s);
                };
                let rel = file_dir(from_file).join(path);
                self.in_file(std::path::Path::new(path), name)
                    .or_else(|| self.in_file(&rel, name))
                    .map_or_else(|| self.unique(name), Ok)
            }
            NodeRef::Imported { module, name } => {
                let files = Self::module_files(from_file, module);
                if let Some(i) = files.iter().find_map(|f| self.in_file(f, name)) {
                    return Ok(i);
                }
                match self.unique(name) {
                    // Name the file the import reaches, not the first path tried (#551).
                    Err(Miss::NotFound) => Err(Miss::NotInModule {
                        module: module.clone(),
                        file: norm_path(files.iter().find(|f| f.is_file()).unwrap_or(&files[0])),
                    }),
                    other => other,
                }
            }
        }
    }

    /// `resolve`, as the DagError for input `param` of `node`.
    pub(crate) fn resolve_input(
        &self,
        node: &ExtractedNode,
        param: &str,
        r: &barca_model::model::NodeRef,
    ) -> Result<usize, DagError> {
        self.resolve(&node.source_file, r).map_err(|miss| {
            let node_id = node.continuity_key();
            let upstream = match r {
                barca_model::model::NodeRef::Canonical(s) => s.clone(),
                other => other.resolution_name().to_string(),
            };
            match miss {
                Miss::NotFound => DagError::UnresolvedInput {
                    node: node_id,
                    param: param.to_string(),
                    upstream,
                },
                Miss::Ambiguous(ids) => DagError::AmbiguousInput {
                    node: node_id,
                    param: param.to_string(),
                    upstream,
                    candidates: ids.join(", "),
                },
                Miss::NotInModule { module, file } => DagError::UnresolvedImport {
                    node: node_id,
                    param: param.to_string(),
                    name: upstream,
                    module,
                    file,
                },
            }
        })
    }
}
