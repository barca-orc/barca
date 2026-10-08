//! Which arguments barca's decorators and helpers take, and the plan-time check that a
//! pipeline passes no others (#284).
//!
//! An argument barca does not define used to be ignored: `@asset(after=other)` or a misspelt
//! `input=` planned and ran with exit 0 and did nothing. [`SIGNATURES`] is the one list of what
//! each call accepts. The check below reads it, the tests at the bottom hold the Python stubs
//! (`python/barca/__init__.py`) and the tables in the manual and on the site to it.
//!
//! The check works on the decorators of one function, already parsed: no second parse.

use ruff_python_ast::visitor::{self, Visitor};
use ruff_python_ast::{self as ast, Expr};
use ruff_text_size::Ranged;

/// What one barca call accepts.
pub struct Signature {
    /// The name as written in a pipeline: `asset`, `partitions`, `Schedule`.
    pub name: &'static str,
    /// Whether it is written with `@`.
    pub decorator: bool,
    /// How many positional arguments it takes at most.
    pub positional: usize,
    /// The keyword arguments it accepts, in the order of the Python signature.
    pub keywords: &'static [&'static str],
    /// Keywords that are not accepted but are rejected later with a message of their own.
    deferred: &'static [&'static str],
    /// The call written out, for the fix.
    pub usage: &'static str,
    /// The manual topic that describes it.
    pub topic: &'static str,
}

/// Every barca call whose arguments are read from the source. `@unsafe` takes no call, and
/// `parallel()` / `parallel_map()` run inside a task body: their arguments are yours.
pub const SIGNATURES: &[Signature] = &[
    Signature {
        name: "asset",
        decorator: true,
        positional: 0,
        keywords: &[
            "name",
            "inputs",
            "partitions",
            "serializer",
            "freshness",
            "timeout_seconds",
            "retries",
            "retry_backoff",
            "description",
            "tags",
            "env",
        ],
        deferred: &[],
        usage: "@asset(inputs={\"param\": upstream}, ...)",
        topic: "assets",
    },
    Signature {
        name: "sensor",
        decorator: true,
        positional: 0,
        keywords: &[
            "name",
            "freshness",
            "timeout_seconds",
            "retries",
            "retry_backoff",
            "description",
            "tags",
            "env",
        ],
        // "sensor '...' cannot have inputs", when the DAG is built.
        deferred: &["inputs"],
        usage: "@sensor(freshness=Schedule(\"<cron>\"), ...)",
        topic: "assets",
    },
    Signature {
        name: "task",
        decorator: true,
        positional: 0,
        keywords: &[
            "name",
            "inputs",
            "freshness",
            "timeout_seconds",
            "retries",
            "retry_backoff",
            "description",
            "tags",
            "env",
        ],
        deferred: &[],
        usage: "@task(inputs={\"param\": upstream}, ...)",
        topic: "tasks",
    },
    Signature {
        name: "sink",
        decorator: true,
        positional: 1,
        keywords: &["serializer"],
        deferred: &[],
        usage: "@sink(\"path/to/file.json\", serializer=\"json\")",
        topic: "sinks",
    },
    Signature {
        name: "partitions",
        decorator: false,
        positional: 1,
        keywords: &[],
        deferred: &[],
        usage: "partitions([\"a\", \"b\"])",
        topic: "partitions",
    },
    Signature {
        name: "partitions_from",
        decorator: false,
        positional: 1,
        keywords: &[],
        deferred: &[],
        usage: "partitions_from(upstream)",
        topic: "partitions",
    },
    Signature {
        name: "collect",
        decorator: false,
        positional: 1,
        keywords: &[],
        deferred: &[],
        usage: "collect(upstream)",
        topic: "partitions",
    },
    Signature {
        name: "asset_ref",
        decorator: false,
        positional: 1,
        keywords: &[],
        deferred: &[],
        usage: "asset_ref(\"file.py:name\")",
        topic: "assets",
    },
    Signature {
        name: "Schedule",
        decorator: false,
        positional: 1,
        keywords: &[],
        deferred: &[],
        usage: "Schedule(\"0 5 * * *\")",
        topic: "scheduling",
    },
];

impl Signature {
    pub fn named(name: &str) -> Option<&'static Signature> {
        SIGNATURES.iter().find(|s| s.name == name)
    }

    /// `@asset` or `partitions()`.
    pub fn display(&self) -> String {
        if self.decorator {
            format!("@{}", self.name)
        } else {
            format!("{}()", self.name)
        }
    }

    /// "`@asset` accepts: `name`, `inputs`" or "`collect()` takes no keyword arguments".
    pub fn accepts(&self) -> String {
        if self.keywords.is_empty() {
            format!("{} takes no keyword arguments", self.display())
        } else {
            format!("{} accepts: {}", self.display(), self.keywords.join(", "))
        }
    }

    /// The row of this call in the tables of the manual and the site.
    pub fn doc_row(&self) -> String {
        let keywords = if self.keywords.is_empty() {
            "none".to_string()
        } else {
            self.keywords
                .iter()
                .map(|k| format!("`{k}`"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let positional = match self.positional {
            0 => "none",
            _ => "one",
        };
        format!("| `{}` | {positional} | {keywords} |", self.display())
    }

    /// The accepted keyword the user most likely meant, when exactly one is close enough for
    /// the guess to be safe: one edit away, or two for a name longer than four characters
    /// (`input` -> `inputs`, `serialiser` -> `serializer`). Two candidates give no suggestion.
    pub fn closest(&self, unknown: &str) -> Option<&'static str> {
        let limit = if unknown.chars().count() > 4 { 2 } else { 1 };
        let mut close = self
            .keywords
            .iter()
            .filter(|k| edit_distance(unknown, k) <= limit);
        match (close.next(), close.next()) {
            (Some(only), None) => Some(only),
            _ => None,
        }
    }
}

/// Optimal string alignment distance: insertions, deletions, substitutions and swaps of two
/// neighbouring characters each count one.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    d[0] = (0..=b.len()).collect();
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[a.len()][b.len()]
}

/// One argument a barca call does not take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// Byte offset of the argument in the source, for the line number.
    pub offset: usize,
    /// What is wrong, ending with what the call accepts.
    pub message: String,
    /// What to do.
    pub fix: String,
}

fn check_call(call: &ast::ExprCall, sig: &Signature) -> Option<Problem> {
    let shown = sig.display();
    let see = format!("See `barca docs {}`.", sig.topic);

    for arg in call.arguments.args.iter() {
        if let Expr::Starred(star) = arg
            && sig.decorator
        {
            return Some(Problem {
                offset: star.range().start().to_usize(),
                message: format!(
                    "{shown} is called with `*` arguments. barca reads decorator arguments \
                     from the source without running it, so it cannot see what they are"
                ),
                fix: format!("Write the arguments out, like `{}`. {see}", sig.usage),
            });
        }
    }
    if sig.decorator && call.arguments.args.len() > sig.positional {
        let extra = &call.arguments.args[sig.positional];
        let takes = match sig.positional {
            0 => "takes keyword arguments only".to_string(),
            _ => "takes one positional argument".to_string(),
        };
        return Some(Problem {
            offset: extra.range().start().to_usize(),
            message: format!(
                "{shown} {takes}, and is called with {}. {}",
                match call.arguments.args.len() {
                    1 => "a positional argument".to_string(),
                    n => format!("{n} positional arguments"),
                },
                sig.accepts()
            ),
            fix: format!(
                "Pass it by keyword, like `{}`, or remove it. {see}",
                sig.usage
            ),
        });
    }

    for kw in call.arguments.keywords.iter() {
        let offset = kw.range().start().to_usize();
        let Some(name) = kw.arg.as_ref().map(|a| a.as_str()) else {
            return Some(Problem {
                offset,
                message: format!(
                    "{shown} is called with `**` arguments. barca reads decorator arguments \
                     from the source without running it, so it cannot see what they are. {}",
                    sig.accepts()
                ),
                fix: format!("Write the arguments out, like `{}`. {see}", sig.usage),
            });
        };
        if sig.keywords.contains(&name) || sig.deferred.contains(&name) {
            continue;
        }
        let guess = sig.closest(name);
        let meant = guess
            .map(|g| format!(" Did you mean `{g}`?"))
            .unwrap_or_default();
        let fix = if sig.keywords.is_empty() {
            format!(
                "Pass the value by position, like `{}`, or remove `{name}`. {see}",
                sig.usage
            )
        } else {
            match guess {
                Some(g) => format!("Rename `{name}` to `{g}`, or remove it. {see}"),
                None => format!(
                    "Remove `{name}`, or replace it with an argument {shown} accepts. {see}"
                ),
            }
        };
        return Some(Problem {
            offset,
            message: format!(
                "`{name}` is not an argument of {shown}.{meant} {}",
                sig.accepts()
            ),
            fix,
        });
    }
    None
}

/// Check every barca call in the decorators of one function: the decorators themselves and
/// the helpers inside their arguments (`partitions(...)`, `collect(...)`, `Schedule(...)`).
/// Returns the first problem, in source order.
///
/// `is_barca(name)` says whether `name` is barca's in this file. A name the file defines or
/// imports from somewhere else is not checked: its arguments are that function's business.
pub fn check_decorators(
    decorators: &[ast::Decorator],
    is_barca: &dyn Fn(&str) -> bool,
) -> Option<Problem> {
    /// The helper calls inside a decorator's arguments.
    struct Helpers<'f> {
        is_barca: &'f dyn Fn(&str) -> bool,
        found: Option<Problem>,
    }
    impl<'a> Visitor<'a> for Helpers<'_> {
        fn visit_expr(&mut self, expr: &'a Expr) {
            if self.found.is_some() {
                return;
            }
            if let Expr::Call(call) = expr
                && let Expr::Name(n) = call.func.as_ref()
                && let Some(sig) = Signature::named(n.id.as_str())
                && !sig.decorator
                && (self.is_barca)(sig.name)
            {
                self.found = check_call(call, sig);
                if self.found.is_some() {
                    return;
                }
            }
            visitor::walk_expr(self, expr);
        }
    }

    for decorator in decorators {
        // Only a call to one of barca's decorators is looked at, and only its arguments.
        let Expr::Call(call) = &decorator.expression else {
            continue;
        };
        let Expr::Name(n) = call.func.as_ref() else {
            continue;
        };
        let Some(sig) = Signature::named(n.id.as_str()).filter(|s| s.decorator) else {
            continue;
        };
        if !is_barca(sig.name) {
            continue;
        }
        if let Some(problem) = check_call(call, sig) {
            return Some(problem);
        }
        let mut helpers = Helpers {
            is_barca,
            found: None,
        };
        visitor::walk_arguments(&mut helpers, &call.arguments);
        if helpers.found.is_some() {
            return helpers.found;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruff_python_ast::Stmt;
    use std::path::{Path, PathBuf};

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap()
    }

    #[test]
    fn suggestions_are_offered_only_when_one_name_is_close() {
        let asset = Signature::named("asset").unwrap();
        assert_eq!(asset.closest("input"), Some("inputs"));
        assert_eq!(asset.closest("partition"), Some("partitions"));
        assert_eq!(asset.closest("serialiser"), Some("serializer"));
        assert_eq!(asset.closest("freshnes"), Some("freshness"));
        assert_eq!(asset.closest("tag"), Some("tags"));
        assert_eq!(asset.closest("evn"), Some("env")); // two neighbours swapped
        // Nothing close: no guess.
        assert_eq!(asset.closest("after"), None);
        assert_eq!(asset.closest("when"), None);
        assert_eq!(asset.closest("timeout"), None);
        // `retry` is as far from `retries` as from nothing else, but three edits: no guess.
        assert_eq!(asset.closest("retry"), None);
        // A short name gets one edit only: `tab` is two from `tags`.
        assert_eq!(asset.closest("tab"), None);
        // The helpers take no keywords, so there is nothing to suggest.
        assert_eq!(Signature::named("collect").unwrap().closest("asset"), None);
    }

    #[test]
    fn two_close_names_give_no_suggestion() {
        let sig = Signature {
            name: "x",
            decorator: true,
            positional: 0,
            keywords: &["inputs", "input_s"],
            deferred: &[],
            usage: "",
            topic: "",
        };
        assert_eq!(sig.closest("input"), None);
    }

    /// Parameters of a stub: (positional-only, accepted by keyword), `self` and the decorated
    /// function `fn` left out. Panics on `*args` / `**kwargs`: a stub that takes anything
    /// would run code that barca rejects.
    fn stub_parameters(name: &str, params: &ast::Parameters) -> (Vec<String>, Vec<String>) {
        assert!(
            params.vararg.is_none() && params.kwarg.is_none(),
            "the stub `{name}` takes *args or **kwargs: it would accept arguments barca rejects"
        );
        let own = |n: &str| n != "self" && n != "fn";
        let positional = params
            .posonlyargs
            .iter()
            .map(|p| p.name().to_string())
            .filter(|n| own(n))
            .collect();
        let keywords = params
            .args
            .iter()
            .chain(&params.kwonlyargs)
            .map(|p| p.name().to_string())
            .filter(|n| own(n))
            .collect();
        (positional, keywords)
    }

    /// The Python stubs are what a type checker, an IDE and `python pipeline.py` see. Their
    /// signatures must accept exactly what the check accepts.
    #[test]
    fn the_python_stubs_accept_exactly_these_arguments() {
        let path = repo_root().join("python/barca/__init__.py");
        let source = std::fs::read_to_string(&path).unwrap();
        let module = ruff_python_parser::parse_module(&source)
            .unwrap()
            .into_syntax();

        let mut seen = Vec::new();
        for stmt in &module.body {
            let (name, params) = match stmt {
                Stmt::FunctionDef(f) => (f.name.as_str(), &*f.parameters),
                Stmt::ClassDef(c) => {
                    let init = c.body.iter().find_map(|s| match s {
                        Stmt::FunctionDef(f) if f.name.as_str() == "__init__" => Some(f),
                        _ => None,
                    });
                    match init {
                        Some(f) => (c.name.as_str(), &*f.parameters),
                        None => continue,
                    }
                }
                _ => continue,
            };
            let Some(sig) = Signature::named(name) else {
                continue;
            };
            let (positional, keywords) = stub_parameters(name, params);
            assert_eq!(
                keywords, sig.keywords,
                "keyword arguments of `{name}` in python/barca/__init__.py differ from \
                 SIGNATURES in crates/barca-core/src/decorator_args.rs"
            );
            assert_eq!(
                positional.len(),
                sig.positional,
                "positional-only parameters of `{name}` in python/barca/__init__.py \
                 ({positional:?}) differ from SIGNATURES"
            );
            seen.push(name.to_string());
        }
        let expected: Vec<&str> = SIGNATURES.iter().map(|s| s.name).collect();
        seen.sort();
        let mut sorted = expected.clone();
        sorted.sort();
        assert_eq!(
            seen, sorted,
            "every entry of SIGNATURES needs a stub, and one only"
        );
    }

    /// The manual and the site list the accepted arguments in one table, generated from
    /// [`Signature::doc_row`].
    #[test]
    fn the_manual_and_the_site_list_exactly_these_arguments() {
        for file in [
            "crates/barca-cli/docs/assets.md",
            "site/src/content/docs/reference/api/decorators.md",
        ] {
            let text = std::fs::read_to_string(repo_root().join(file)).unwrap();
            let rows: Vec<&str> = text
                .lines()
                .skip_while(|l| !l.starts_with("| Call | Positional arguments |"))
                .skip(2)
                .take_while(|l| l.starts_with('|'))
                .collect();
            let expected: Vec<String> = SIGNATURES.iter().map(Signature::doc_row).collect();
            assert_eq!(
                rows,
                expected,
                "the accepted-arguments table in {file} differs from SIGNATURES. It should \
                 read:\n| Call | Positional arguments | Keyword arguments |\n|---|---|---|\n{}",
                expected.join("\n")
            );
        }
    }
}
