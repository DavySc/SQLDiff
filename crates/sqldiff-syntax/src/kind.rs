//! Token and node kinds of the concrete syntax tree.

#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum SyntaxKind {
    // Tokens.
    WHITESPACE = 0,
    LINE_COMMENT,
    BLOCK_COMMENT,
    /// Unquoted word: keyword or identifier (keywords are recognised by the parser).
    IDENT,
    /// `"x"`, `[x]`, `U&"x"`.
    QUOTED_IDENT,
    /// T-SQL `@x` / `@@x`.
    VARIABLE,
    /// `$1`, `?`.
    PARAM,
    NUMBER,
    STRING,
    /// PostgreSQL `$tag$...$tag$` kept as one token when it is not parsed as a body.
    DOLLAR_STRING,
    /// Opening or closing delimiter of a dollar-quoted body that was parsed.
    DOLLAR_QUOTE,
    OP,
    SEMICOLON,
    COMMA,
    DOT,
    COLON,
    L_PAREN,
    R_PAREN,
    L_BRACKET,
    R_BRACKET,
    L_BRACE,
    R_BRACE,
    /// T-SQL `GO [count]`.
    BATCH_SEP,
    /// Zero-width token: the parser ended a statement that had no `;`.
    INFERRED_TERMINATOR,
    ERROR_TOKEN,

    // Nodes.
    SOURCE_FILE,
    BATCH,
    STATEMENT,
    ROUTINE,
    ROUTINE_BODY,
    BLOCK,
    DECLARE_SECTION,
    EXCEPTION_SECTION,
    EXCEPTION_HANDLER,
    TRY_CATCH,
    TRY_BLOCK,
    CATCH_BLOCK,
    IF_STMT,
    LOOP_STMT,
    CASE_STMT,
    HANDLER,
    CONDITION,
    BRANCH,
    LABEL,
    ERROR,

    #[doc(hidden)]
    __LAST,
}

use SyntaxKind::*;

impl SyntaxKind {
    /// Every kind, in declaration order.
    pub fn all() -> impl Iterator<Item = SyntaxKind> {
        (0..__LAST as u16)
            .map(|raw| <SqlLanguage as rowan::Language>::kind_from_raw(rowan::SyntaxKind(raw)))
    }

    pub fn is_trivia(self) -> bool {
        matches!(self, WHITESPACE | LINE_COMMENT | BLOCK_COMMENT)
    }

    pub fn is_token(self) -> bool {
        (self as u16) < (SOURCE_FILE as u16)
    }

    /// Nodes that stand for one statement (simple or compound).
    pub fn is_unit(self) -> bool {
        matches!(
            self,
            STATEMENT
                | ROUTINE
                | BLOCK
                | TRY_CATCH
                | IF_STMT
                | LOOP_STMT
                | CASE_STMT
                | HANDLER
                | LABEL
                | ERROR
        )
    }

    /// Nodes that hold a list of units.
    pub fn is_container(self) -> bool {
        matches!(
            self,
            SOURCE_FILE
                | BATCH
                | ROUTINE_BODY
                | DECLARE_SECTION
                | EXCEPTION_SECTION
                | EXCEPTION_HANDLER
                | TRY_BLOCK
                | CATCH_BLOCK
                | BRANCH
        )
    }
}

impl From<SyntaxKind> for rowan::SyntaxKind {
    fn from(kind: SyntaxKind) -> Self {
        rowan::SyntaxKind(kind as u16)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SqlLanguage {}

impl rowan::Language for SqlLanguage {
    type Kind = SyntaxKind;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> SyntaxKind {
        assert!(raw.0 < __LAST as u16);
        // SAFETY: SyntaxKind is repr(u16) and the value is in range.
        unsafe { std::mem::transmute::<u16, SyntaxKind>(raw.0) }
    }

    fn kind_to_raw(kind: SyntaxKind) -> rowan::SyntaxKind {
        kind.into()
    }
}

pub type SyntaxNode = rowan::SyntaxNode<SqlLanguage>;
pub type SyntaxToken = rowan::SyntaxToken<SqlLanguage>;
pub type SyntaxElement = rowan::SyntaxElement<SqlLanguage>;
