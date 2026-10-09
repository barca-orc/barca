//! Static organizational metadata, kept separate from dependency edges and hashes.
use crate::{
    BarcaError,
    model::{ExtractedNode, NodeRef},
    parse::FileNames,
};
use ruff_python_ast::{Expr, Stmt};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct NodeGroup {
    pub id: String,
    pub name: String,
    pub description: String,
    pub members: Vec<String>,
    pub output: String,
}

pub(crate) struct Declaration {
    pub(crate) file: String,
    binding: String,
    name: String,
    description: String,
    members: Vec<NodeRef>,
    output: NodeRef,
}
fn invalid(file: &str, reason: impl std::fmt::Display) -> BarcaError {
    BarcaError::Parse(format!(
        "{file}: invalid group: {reason}. See `barca docs groups`."
    ))
}

#[cfg(test)]
pub(crate) fn extract(source: &str, file: &str) -> Result<Vec<Declaration>, BarcaError> {
    let parsed = ruff_python_parser::parse_module(source).map_err(|e| invalid(file, e))?;
    let body = &parsed.syntax().body;
    extract_body(body, file)
}

pub(crate) fn extract_body(body: &[Stmt], file: &str) -> Result<Vec<Declaration>, BarcaError> {
    let names = FileNames::collect(body);
    let mut result = Vec::new();
    for stmt in body {
        let (target, value) = match stmt {
            Stmt::Assign(a) if a.targets.len() == 1 => (&a.targets[0], a.value.as_ref()),
            Stmt::AnnAssign(a) if a.value.is_some() => {
                (a.target.as_ref(), a.value.as_ref().unwrap().as_ref())
            }
            _ => continue,
        };
        let Expr::Call(call) = value else { continue };
        if names.barca.resolve(&call.func) != Some("group") {
            continue;
        }
        if call
            .arguments
            .args
            .iter()
            .any(|a| matches!(a, Expr::Starred(_)))
            || call.arguments.keywords.iter().any(|k| k.arg.is_none())
        {
            return Err(invalid(file, "starred arguments are not supported"));
        }
        let Expr::Name(binding) = target else {
            return Err(invalid(file, "assign group(...) to a single name"));
        };
        if let Some(problem) = crate::decorator_args::check_call(
            call,
            crate::decorator_args::Signature::named("group").unwrap(),
        ) {
            return Err(invalid(file, problem.message));
        }
        let Expr::StringLiteral(name) = &call.arguments.args[0] else {
            return Err(invalid(file, "name must be a literal string"));
        };
        if name.value.to_str().trim().is_empty() {
            return Err(invalid(file, "name must not be empty"));
        }
        let keyword = |name: &str| {
            call.arguments
                .keywords
                .iter()
                .find(|k| k.arg.as_ref().is_some_and(|a| a.as_str() == name))
                .map(|k| &k.value)
        };
        let member_exprs = match keyword("members") {
            Some(Expr::List(l)) => &l.elts,
            Some(Expr::Tuple(t)) => &t.elts,
            _ => {
                return Err(invalid(
                    file,
                    "members must be a literal list or tuple of node/group references",
                ));
            }
        };
        if member_exprs.is_empty() {
            return Err(invalid(file, "members must not be empty"));
        }
        let reference = |expr: &Expr| {
            names.node_ref(expr).ok_or_else(|| {
                invalid(
                    file,
                    "use node or group references, not expressions or strings",
                )
            })
        };
        let members = member_exprs
            .iter()
            .map(reference)
            .collect::<Result<Vec<_>, _>>()?;
        let output = keyword("output")
            .ok_or_else(|| invalid(file, "output is required"))
            .and_then(reference)?;
        let description = match keyword("description") {
            None => String::new(),
            Some(Expr::StringLiteral(s)) => s.value.to_str().to_string(),
            _ => return Err(invalid(file, "description must be a literal string")),
        };
        result.push(Declaration {
            file: file.into(),
            binding: binding.id.to_string(),
            name: name.value.to_str().into(),
            description,
            members,
            output,
        });
    }
    Ok(result)
}

pub(crate) fn resolve(
    declarations: Vec<Declaration>,
    nodes: &[ExtractedNode],
) -> Result<Vec<NodeGroup>, BarcaError> {
    let mut bindings = HashMap::new();
    for d in &declarations {
        if nodes
            .iter()
            .any(|n| n.source_file == d.file && n.function_name == d.binding)
        {
            return Err(invalid(
                &d.file,
                "a group cannot replace a node's function binding",
            ));
        }
        let id = format!("group:{}:{}", d.file, d.binding);
        if bindings
            .insert((d.file.clone(), d.binding.clone()), id)
            .is_some()
        {
            return Err(invalid(
                &d.file,
                format!("duplicate group binding {}", d.binding),
            ));
        }
    }
    let resolver = crate::dag::Resolver::new(nodes);
    let mut groups = Vec::new();
    for d in declarations {
        let reference = |r: &NodeRef| -> Result<String, BarcaError> {
            if let NodeRef::FunctionName(name) = r
                && let Some(id) = bindings.get(&(d.file.clone(), name.clone()))
            {
                return Ok(id.clone());
            }
            resolver
                .resolve_id(&d.file, r)
                .ok_or_else(|| invalid(&d.file, format!("unknown or ambiguous member {r:?}")))
        };
        groups.push(NodeGroup {
            id: bindings[&(d.file.clone(), d.binding.clone())].clone(),
            name: d.name.clone(),
            description: d.description.clone(),
            members: d.members.iter().map(reference).collect::<Result<_, _>>()?,
            output: reference(&d.output)?,
        });
    }
    let by_id: HashMap<_, _> = groups.iter().map(|g| (g.id.as_str(), g)).collect();
    let mut owners = HashMap::new();
    for g in &groups {
        for m in &g.members {
            if let Some(owner) = owners.insert(m, &g.id) {
                return Err(invalid(
                    &g.id,
                    format!("member {m} belongs to both {owner} and {}", g.id),
                ));
            }
        }
    }
    fn descendants<'a>(
        id: &'a str,
        groups: &HashMap<&'a str, &'a NodeGroup>,
        visiting: &mut HashSet<&'a str>,
    ) -> Result<HashSet<&'a str>, BarcaError> {
        if !visiting.insert(id) {
            return Err(invalid(id, "membership cycle"));
        }
        let mut all = HashSet::new();
        if let Some(g) = groups.get(id) {
            for m in &g.members {
                all.insert(m.as_str());
                all.extend(descendants(m, groups, visiting)?);
            }
        }
        visiting.remove(id);
        Ok(all)
    }
    for g in &groups {
        if !descendants(&g.id, &by_id, &mut HashSet::new())?.contains(g.output.as_str()) {
            return Err(invalid(
                &g.id,
                "output must be a member or descendant of the group",
            ));
        }
    }
    Ok(groups)
}

#[cfg(test)]
mod tests {
    use super::*;
    const BASE: &str = "from barca import asset, task, group\n@asset\ndef rows(): return 1\n@asset(inputs={'x': rows})\ndef model(x): return x\n@task(inputs={'x': model})\ndef check(x): pass\n";
    fn parse(source: &str) -> Result<Vec<NodeGroup>, BarcaError> {
        resolve(
            extract(source, "p.py")?,
            &crate::parse::extract_nodes(source, "p.py").unwrap(),
        )
    }
    #[test]
    fn nested_groups_and_descendant_outputs() {
        let source = format!(
            "{BASE}\ninner = group('Preparation', members=[rows], output=rows)\nouter = group('Training', members=[inner, model, check], output=model)\nroot = group('root', members=[outer], output=rows)\n"
        );
        let g = parse(&source).unwrap();
        assert_eq!(g.len(), 3);
        assert_eq!(
            g[1].members,
            ["group:p.py:inner", "p.py:model", "p.py:check"]
        );
        assert_eq!(g[1].output, "p.py:model");
    }
    #[test]
    fn rejects_invalid_hierarchies_and_dynamic_metadata() {
        for declaration in [
            "g = group('g', members=[missing], output=missing)",
            "g = group('g', members=[rows], output=model)",
            "g = group('g', members=[g], output=g)",
            "a = group('a', members=[b], output=b)\nb = group('b', members=[a], output=a)",
            "a = group('a', members=[rows], output=rows)\nb = group('b', members=[rows], output=rows)",
            "a = group('a', members=[rows, rows], output=rows)",
            "a = group('a', members=make_members(), output=rows)",
            "a = group('a', members=[], output=rows)",
            "a = group('a', members=['rows'], output=rows)",
            "a = group('a', members=[rows])",
            "a = group('a', members=[rows], output=rows, typo=True)",
            "a = group('a', members=[rows], output=rows, **kwargs)",
        ] {
            assert!(
                parse(&format!("{BASE}\n{declaration}\n")).is_err(),
                "{declaration}"
            );
        }
    }
    #[test]
    fn imported_alias_and_qualified_helper_only() {
        assert_eq!(parse("import barca as b\n@b.asset\ndef x(): return 1\ng = b.group('g', members=[x], output=x)\n").unwrap().len(), 1);
        assert_eq!(parse("from barca import asset, group as folder\n@asset\ndef x(): return 1\ng = folder('g', members=[x], output=x)\n").unwrap().len(), 1);
        assert!(
            parse("from elsewhere import group\ng = group('foreign')\n")
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn group_names_matching_local_variables_do_not_change_hashes() {
        let source = "from barca import asset, group\n@asset\ndef rows(): return 1\n@asset(inputs={'source': rows})\ndef model(source): return source\n";
        let grouped =
            format!("{source}\nsource = group('data', members=[rows, model], output=model)\n");
        let modules: HashMap<String, std::rc::Rc<crate::cone::Module>> = HashMap::new();
        let a = crate::cone::Module::new("p", source, false);
        let b = crate::cone::Module::new("p", grouped.as_str(), false);
        assert_eq!(
            crate::cone::cone_hash(&a, "model", &modules),
            crate::cone::cone_hash(&b, "model", &modules)
        );
        let demo = include_str!("../../../ui/demo/modeling.py");
        let original = demo
            .split("# Organizational hierarchy")
            .next()
            .unwrap()
            .replace("asset, group, task", "asset, task");
        let a = crate::cone::Module::new("modeling", original.as_str(), false);
        let b = crate::cone::Module::new("modeling", demo, false);
        for n in crate::parse::extract_nodes(&original, "modeling.py").unwrap() {
            assert_eq!(
                crate::cone::cone_hash(&a, &n.function_name, &modules),
                crate::cone::cone_hash(&b, &n.function_name, &modules),
                "{}",
                n.function_name
            );
        }
    }

    #[test]
    fn grouping_preserves_dependency_graph_and_hashes() {
        let grouped =
            format!("{BASE}\ng = group('Training', members=[rows, model, check], output=model)\n");
        let ungrouped = BASE.replace(", group", "");
        let before = crate::parse::extract_nodes(&ungrouped, "p.py").unwrap();
        let after = crate::parse::extract_nodes(&grouped, "p.py").unwrap();
        let baseline = crate::cone::Module::new("p", ungrouped.as_str(), false);
        let organizational = crate::cone::Module::new("p", grouped.as_str(), false);
        let modules: HashMap<String, std::rc::Rc<crate::cone::Module>> = HashMap::new();
        for a in &before {
            assert_eq!(
                crate::cone::cone_hash(&baseline, &a.function_name, &modules),
                crate::cone::cone_hash(&organizational, &a.function_name, &modules)
            );
        }
        let a = crate::Dag::build(&before).unwrap();
        let b = crate::Dag::build(&after).unwrap();
        assert_eq!(a.topo_order(), b.topo_order());
        assert_eq!(a.edge_count(), b.edge_count());
        for id in a.topo_order() {
            assert_eq!(a.upstream(id), b.upstream(id));
        }
    }
}
