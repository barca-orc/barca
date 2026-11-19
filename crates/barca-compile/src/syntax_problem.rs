//! Where a Python file stops parsing, as a person reads it. A leaf: the parser ([`crate::parse`])
//! and the dependency cone ([`crate::cone`], [`crate::project_modules`]) both report one.

/// Where a file stops parsing, and why: `line 12, column 18: Expected ':', found newline`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SyntaxProblem {
    /// 1-based.
    pub line: usize,
    /// 1-based, in characters.
    pub column: usize,
    /// The parser's message.
    pub message: String,
    /// The text of that line, without its newline.
    pub text: String,
}

impl SyntaxProblem {
    pub(crate) fn of(source: &str, error: &ruff_python_parser::ParseError) -> Self {
        let offset = usize::from(error.location.start()).min(source.len());
        let before = source.get(..offset).unwrap_or(source);
        let line = before.matches('\n').count() + 1;
        let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
        let text = source
            .lines()
            .nth(line - 1)
            .unwrap_or("")
            .trim_end()
            .to_string();
        SyntaxProblem {
            line,
            column,
            message: error.error.to_string(),
            text,
        }
    }

    /// The line, and a caret under the column: what an editor would point at.
    pub fn excerpt(&self) -> String {
        let pad: String = self
            .text
            .chars()
            .take(self.column.saturating_sub(1))
            .map(|c| if c == '\t' { '\t' } else { ' ' })
            .collect();
        format!("    {}\n    {pad}^", self.text)
    }
}

impl std::fmt::Display for SyntaxProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "line {}, column {}: {}",
            self.line, self.column, self.message
        )
    }
}
