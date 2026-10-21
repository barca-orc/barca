//! Static check: which environment variables does a step's code read? (#574)
//!
//! A step's run hash covers the variables it declares with `env=[...]` ([`crate::envdeps`]) and
//! nothing else from the environment. A variable it reads without declaring it can change while
//! the step stays `cached`, so barca reports those reads, without changing any hash.
//!
//! The reads are found in the step's function and in every definition of its dependency cone
//! ([`crate::cone`]: the helper code the function reaches, in the project), from source text
//! alone. Nothing is imported. What counts as a read, where `NAME` is a string literal:
//!
//! - `os.environ["NAME"]`, `os.environ.get("NAME", ...)`, `os.environ.setdefault("NAME", ...)`,
//!   `os.environ.pop("NAME", ...)`, `"NAME" in os.environ`, also through `from os import
//!   environ` (any expression ending in `.environ`, or the bare name `environ`);
//! - `os.getenv("NAME", ...)` and a bare `getenv("NAME", ...)`;
//! - the fields of a pydantic settings class (`class S(BaseSettings)`): `field: T` reads
//!   `FIELD`, with the class's `env_prefix` (`model_config = SettingsConfigDict(env_prefix=...)`
//!   or `class Config: env_prefix = ...`) in front, and `Field(env="NAME")`,
//!   `Field(validation_alias="NAME")` or `Field(alias="NAME")` reads `NAME`.
//!
//! A name built from an f-string with at least one literal part (`f"{name.upper()}_CSV"`) is
//! reported as a pattern, the interpolations written as `*` (`*_CSV`); a declared name that
//! matches the pattern counts as declaring it. A name that is not literal at all
//! (`os.environ[key]`) is not reported. Writes (`os.environ["NAME"] = ...`) are not reads.
//! Variables whose names start with `BARCA_` are barca's own and are never reported.

use std::collections::BTreeSet;

use ruff_python_ast::visitor::{self, Visitor};
use ruff_python_ast::{self as ast, CmpOp, Expr, ExprContext, Stmt};
use ruff_python_parser::parse_module;

/// Names with this prefix are barca's own settings and worker variables.
const OWN_PREFIX: &str = "BARCA_";

/// The variables `source` (a module, or one top-level definition of one) reads, sorted. Empty
/// when it does not parse.
pub fn in_source(source: &str) -> BTreeSet<String> {
    match parse_module(source) {
        Ok(parsed) => in_body(&parsed.syntax().body),
        Err(_) => BTreeSet::new(),
    }
}

/// The variables the statements read, sorted.
pub fn in_body(body: &[Stmt]) -> BTreeSet<String> {
    let mut reads = Reads::default();
    visitor::walk_body(&mut reads, body);
    reads.names
}

/// The variables a function definition reads (its decorators are not looked at).
pub fn in_function(func: &ast::StmtFunctionDef) -> BTreeSet<String> {
    let mut reads = Reads::default();
    for param in func.parameters.iter_non_variadic_params() {
        if let Some(default) = &param.default {
            reads.visit_expr(default);
        }
    }
    visitor::walk_body(&mut reads, &func.body);
    reads.names
}

/// The names of `reads` that `declared` does not cover, sorted: a literal name is covered when
/// it is declared, a pattern (`*_CSV`) when a declared name matches it.
pub fn undeclared(reads: &BTreeSet<String>, declared: &[String]) -> Vec<String> {
    reads
        .iter()
        .filter(|name| !declared.iter().any(|d| matches(name, d)))
        .cloned()
        .collect()
}

/// Does the literal `name` match `pattern`, where `*` in the pattern stands for any text?
pub fn matches(pattern: &str, name: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or("");
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    let Some((last, middle)) = parts.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

#[derive(Default)]
struct Reads {
    names: BTreeSet<String>,
}

impl Reads {
    fn add(&mut self, expr: &Expr) {
        if let Some(name) = literal_name(expr)
            && !name.starts_with(OWN_PREFIX)
        {
            self.names.insert(name);
        }
    }

    fn add_name(&mut self, name: String) {
        if !name.is_empty() && !name.starts_with(OWN_PREFIX) {
            self.names.insert(name);
        }
    }

    fn settings_class(&mut self, class: &ast::StmtClassDef) {
        let is_settings = class.bases().iter().any(|base| match base {
            Expr::Name(n) => n.id.as_str().ends_with("BaseSettings"),
            Expr::Attribute(a) => a.attr.as_str().ends_with("BaseSettings"),
            _ => false,
        });
        if !is_settings {
            return;
        }
        let prefix = settings_prefix(&class.body).unwrap_or_default();
        for stmt in &class.body {
            let Stmt::AnnAssign(assign) = stmt else {
                continue;
            };
            let Expr::Name(target) = assign.target.as_ref() else {
                continue;
            };
            let field = target.id.as_str();
            if field == "model_config" || field.starts_with('_') || is_class_var(&assign.annotation)
            {
                continue;
            }
            let alias = assign.value.as_deref().and_then(|value| match value {
                Expr::Call(call) => ["env", "validation_alias", "alias"]
                    .iter()
                    .find_map(|k| call.arguments.find_keyword(k))
                    .and_then(|kw| string(&kw.value)),
                _ => None,
            });
            match alias {
                Some(alias) => self.add_name(alias),
                None => self.add_name(format!("{prefix}{field}").to_ascii_uppercase()),
            }
        }
    }
}

impl<'a> Visitor<'a> for Reads {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        if let Stmt::ClassDef(class) = stmt {
            self.settings_class(class);
        }
        visitor::walk_stmt(self, stmt);
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            // os.environ["NAME"]
            Expr::Subscript(sub) if sub.ctx == ExprContext::Load && is_environ(&sub.value) => {
                self.add(&sub.slice);
            }
            Expr::Call(call) => {
                let first = call.arguments.args.first();
                match call.func.as_ref() {
                    // os.environ.get("NAME"), .setdefault, .pop
                    Expr::Attribute(attr)
                        if matches!(attr.attr.as_str(), "get" | "setdefault" | "pop")
                            && is_environ(&attr.value) =>
                    {
                        if let Some(first) = first {
                            self.add(first);
                        }
                    }
                    // os.getenv("NAME"), getenv("NAME")
                    Expr::Attribute(attr) if attr.attr.as_str() == "getenv" => {
                        if let Some(first) = first {
                            self.add(first);
                        }
                    }
                    Expr::Name(name) if name.id.as_str() == "getenv" => {
                        if let Some(first) = first {
                            self.add(first);
                        }
                    }
                    _ => {}
                }
            }
            // "NAME" in os.environ
            Expr::Compare(cmp) => {
                let mut left = cmp.left.as_ref();
                for (op, right) in cmp.ops.iter().zip(cmp.comparators.iter()) {
                    if matches!(op, CmpOp::In | CmpOp::NotIn) && is_environ(right) {
                        self.add(left);
                    }
                    left = right;
                }
            }
            _ => {}
        }
        visitor::walk_expr(self, expr);
    }
}

/// `os.environ`, `environ`, or anything else ending in `.environ`.
fn is_environ(expr: &Expr) -> bool {
    match expr {
        Expr::Name(n) => n.id.as_str() == "environ",
        Expr::Attribute(a) => a.attr.as_str() == "environ",
        _ => false,
    }
}

/// A string literal's value.
fn string(expr: &Expr) -> Option<String> {
    match expr {
        Expr::StringLiteral(s) => Some(s.value.to_str().to_string()),
        _ => None,
    }
}

/// A literal variable name, or a pattern for an f-string with at least one literal part.
fn literal_name(expr: &Expr) -> Option<String> {
    if let Some(name) = string(expr) {
        return (!name.is_empty()).then_some(name);
    }
    let Expr::FString(f) = expr else {
        return None;
    };
    let mut pattern = String::new();
    let mut literal = false;
    for part in &f.value {
        match part {
            ast::FStringPart::Literal(s) => {
                literal |= !s.value.is_empty();
                pattern.push_str(&s.value);
            }
            ast::FStringPart::FString(fstr) => {
                for element in &fstr.elements {
                    match element {
                        ast::InterpolatedStringElement::Literal(l) => {
                            literal |= !l.value.is_empty();
                            pattern.push_str(&l.value);
                        }
                        ast::InterpolatedStringElement::Interpolation(_) => {
                            if !pattern.ends_with('*') {
                                pattern.push('*');
                            }
                        }
                    }
                }
            }
        }
    }
    literal.then_some(pattern)
}

/// A pydantic settings class's `env_prefix`.
fn settings_prefix(body: &[Stmt]) -> Option<String> {
    for stmt in body {
        match stmt {
            Stmt::Assign(assign)
                if assign
                    .targets
                    .iter()
                    .any(|t| matches!(t, Expr::Name(n) if n.id.as_str() == "model_config")) =>
            {
                if let Some(prefix) = prefix_in(&assign.value) {
                    return Some(prefix);
                }
            }
            Stmt::AnnAssign(assign) if matches!(assign.target.as_ref(), Expr::Name(n) if n.id.as_str() == "model_config") => {
                if let Some(prefix) = assign.value.as_deref().and_then(prefix_in) {
                    return Some(prefix);
                }
            }
            Stmt::ClassDef(config) if config.name.as_str() == "Config" => {
                for inner in &config.body {
                    if let Stmt::Assign(assign) = inner
                        && assign
                            .targets
                            .iter()
                            .any(|t| matches!(t, Expr::Name(n) if n.id.as_str() == "env_prefix"))
                        && let Some(prefix) = string(&assign.value)
                    {
                        return Some(prefix);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// `env_prefix` in `SettingsConfigDict(env_prefix="P")` or `{"env_prefix": "P"}`.
fn prefix_in(value: &Expr) -> Option<String> {
    match value {
        Expr::Call(call) => call
            .arguments
            .find_keyword("env_prefix")
            .and_then(|kw| string(&kw.value)),
        Expr::Dict(dict) => dict.items.iter().find_map(|item| {
            let key = item.key.as_ref().and_then(string)?;
            (key == "env_prefix").then(|| string(&item.value)).flatten()
        }),
        _ => None,
    }
}

/// `ClassVar[...]` or `typing.ClassVar[...]`: not a settings field.
fn is_class_var(annotation: &Expr) -> bool {
    let base = match annotation {
        Expr::Subscript(s) => s.value.as_ref(),
        other => other,
    };
    match base {
        Expr::Name(n) => n.id.as_str() == "ClassVar",
        Expr::Attribute(a) => a.attr.as_str() == "ClassVar",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reads(source: &str) -> Vec<String> {
        in_source(source).into_iter().collect()
    }

    #[test]
    fn os_environ_and_getenv_with_literal_names() {
        let src = r#"
import os
from os import environ, getenv

def f(key):
    a = os.environ["A"]
    b = os.environ.get("B", "x")
    c = os.getenv("C")
    d = environ["D"]
    e = getenv("E", "1")
    if "F" in os.environ and "G" not in environ:
        pass
    os.environ.setdefault("H", "1")
    os.environ["WRITTEN"] = "1"
    del os.environ["DELETED"]
    os.environ[key]
    os.environ.get(key)
    os.environ.get("BARCA_RUN_ID")
"#;
        assert_eq!(reads(src), ["A", "B", "C", "D", "E", "F", "G", "H"]);
    }

    #[test]
    fn an_fstring_name_is_a_pattern_and_a_fully_dynamic_one_is_skipped() {
        let src = r#"
import os
def load(name):
    os.environ.get(f"{name.upper()}_REGISTRY_CSV")
    os.environ.get(f"{name}")
    os.getenv(f"APP_{a}{b}_X")
"#;
        assert_eq!(reads(src), ["*_REGISTRY_CSV", "APP_*_X"]);
    }

    #[test]
    fn pydantic_settings_fields_read_prefixed_names() {
        let src = r#"
from typing import ClassVar
from pydantic import Field
from pydantic_settings import BaseSettings, SettingsConfigDict

class Settings(BaseSettings):
    model_config = SettingsConfigDict(env_prefix="app_")
    region: str = "eu"
    token: str = Field(validation_alias="SERVICE_TOKEN")
    VERSION: ClassVar[int] = 1
    _private: int = 0

class Old(pydantic.BaseSettings):
    url: str
    class Config:
        env_prefix = "OLD_"

class Plain:
    field: str = "not settings"
"#;
        assert_eq!(reads(src), ["APP_REGION", "OLD_URL", "SERVICE_TOKEN"]);
    }

    #[test]
    fn reads_in_defaults_count_and_unparseable_source_reads_nothing() {
        let src = "import os\ndef f(x=os.environ.get('DEFAULTED')):\n    return x\n";
        let parsed = parse_module(src).unwrap();
        let Stmt::FunctionDef(func) = &parsed.syntax().body[1] else {
            panic!()
        };
        assert_eq!(
            in_function(func).into_iter().collect::<Vec<_>>(),
            ["DEFAULTED"]
        );
        assert!(in_source("import:helpers:x").is_empty());
    }

    #[test]
    fn declared_names_cover_literals_and_matching_patterns() {
        let reads: BTreeSet<String> = ["A", "B", "*_CSV", "X_*_Y"]
            .into_iter()
            .map(String::from)
            .collect();
        let declared = vec!["A".to_string(), "ACCOUNT_CSV".to_string()];
        assert_eq!(undeclared(&reads, &declared), ["B", "X_*_Y"]);
        assert!(matches("X_*_Y", "X_1_Y"));
        assert!(matches("X_*_Y", "X__Y"));
        assert!(!matches("X_*_Y", "X_Y"));
        assert!(!matches("*_CSV", "CSV"));
        assert!(matches("*", "ANY"));
    }
}
