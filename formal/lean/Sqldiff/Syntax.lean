/-
Generated from `sqldiff_syntax::SyntaxKind` by `sqldiff_core::proof::lean_syntax`.
Do not edit: run `SQLDIFF_BLESS=1 cargo test -p sqldiff-core --test lean_syntax`.
-/

namespace Sqldiff.Syntax

/-- The schema hash requests must carry (`sqldiff_core::proof::schema_hash`). -/
def schemaHash : String := "bf39efb096c573e9"

/-- The kinds of SQLDiff's syntax tree. -/
inductive Kind where
  | WHITESPACE
  | LINE_COMMENT
  | BLOCK_COMMENT
  | IDENT
  | QUOTED_IDENT
  | VARIABLE
  | PARAM
  | NUMBER
  | STRING
  | DOLLAR_STRING
  | DOLLAR_QUOTE
  | OP
  | SEMICOLON
  | COMMA
  | DOT
  | COLON
  | L_PAREN
  | R_PAREN
  | L_BRACKET
  | R_BRACKET
  | L_BRACE
  | R_BRACE
  | BATCH_SEP
  | INFERRED_TERMINATOR
  | ERROR_TOKEN
  | SOURCE_FILE
  | BATCH
  | STATEMENT
  | ROUTINE
  | ROUTINE_BODY
  | BLOCK
  | DECLARE_SECTION
  | EXCEPTION_SECTION
  | EXCEPTION_HANDLER
  | TRY_CATCH
  | TRY_BLOCK
  | CATCH_BLOCK
  | IF_STMT
  | LOOP_STMT
  | CASE_STMT
  | HANDLER
  | CONDITION
  | BRANCH
  | LABEL
  | ERROR
deriving DecidableEq, Repr

/-- A kind by its Rust name; `none` for a name this schema lacks. -/
def Kind.ofName? : String → Option Kind
  | "WHITESPACE" => some .WHITESPACE
  | "LINE_COMMENT" => some .LINE_COMMENT
  | "BLOCK_COMMENT" => some .BLOCK_COMMENT
  | "IDENT" => some .IDENT
  | "QUOTED_IDENT" => some .QUOTED_IDENT
  | "VARIABLE" => some .VARIABLE
  | "PARAM" => some .PARAM
  | "NUMBER" => some .NUMBER
  | "STRING" => some .STRING
  | "DOLLAR_STRING" => some .DOLLAR_STRING
  | "DOLLAR_QUOTE" => some .DOLLAR_QUOTE
  | "OP" => some .OP
  | "SEMICOLON" => some .SEMICOLON
  | "COMMA" => some .COMMA
  | "DOT" => some .DOT
  | "COLON" => some .COLON
  | "L_PAREN" => some .L_PAREN
  | "R_PAREN" => some .R_PAREN
  | "L_BRACKET" => some .L_BRACKET
  | "R_BRACKET" => some .R_BRACKET
  | "L_BRACE" => some .L_BRACE
  | "R_BRACE" => some .R_BRACE
  | "BATCH_SEP" => some .BATCH_SEP
  | "INFERRED_TERMINATOR" => some .INFERRED_TERMINATOR
  | "ERROR_TOKEN" => some .ERROR_TOKEN
  | "SOURCE_FILE" => some .SOURCE_FILE
  | "BATCH" => some .BATCH
  | "STATEMENT" => some .STATEMENT
  | "ROUTINE" => some .ROUTINE
  | "ROUTINE_BODY" => some .ROUTINE_BODY
  | "BLOCK" => some .BLOCK
  | "DECLARE_SECTION" => some .DECLARE_SECTION
  | "EXCEPTION_SECTION" => some .EXCEPTION_SECTION
  | "EXCEPTION_HANDLER" => some .EXCEPTION_HANDLER
  | "TRY_CATCH" => some .TRY_CATCH
  | "TRY_BLOCK" => some .TRY_BLOCK
  | "CATCH_BLOCK" => some .CATCH_BLOCK
  | "IF_STMT" => some .IF_STMT
  | "LOOP_STMT" => some .LOOP_STMT
  | "CASE_STMT" => some .CASE_STMT
  | "HANDLER" => some .HANDLER
  | "CONDITION" => some .CONDITION
  | "BRANCH" => some .BRANCH
  | "LABEL" => some .LABEL
  | "ERROR" => some .ERROR
  | _ => none

/-- Whether the kind is a token (a leaf with text). -/
def Kind.isToken : Kind → Bool
  | .WHITESPACE => true
  | .LINE_COMMENT => true
  | .BLOCK_COMMENT => true
  | .IDENT => true
  | .QUOTED_IDENT => true
  | .VARIABLE => true
  | .PARAM => true
  | .NUMBER => true
  | .STRING => true
  | .DOLLAR_STRING => true
  | .DOLLAR_QUOTE => true
  | .OP => true
  | .SEMICOLON => true
  | .COMMA => true
  | .DOT => true
  | .COLON => true
  | .L_PAREN => true
  | .R_PAREN => true
  | .L_BRACKET => true
  | .R_BRACKET => true
  | .L_BRACE => true
  | .R_BRACE => true
  | .BATCH_SEP => true
  | .INFERRED_TERMINATOR => true
  | .ERROR_TOKEN => true
  | .SOURCE_FILE => false
  | .BATCH => false
  | .STATEMENT => false
  | .ROUTINE => false
  | .ROUTINE_BODY => false
  | .BLOCK => false
  | .DECLARE_SECTION => false
  | .EXCEPTION_SECTION => false
  | .EXCEPTION_HANDLER => false
  | .TRY_CATCH => false
  | .TRY_BLOCK => false
  | .CATCH_BLOCK => false
  | .IF_STMT => false
  | .LOOP_STMT => false
  | .CASE_STMT => false
  | .HANDLER => false
  | .CONDITION => false
  | .BRANCH => false
  | .LABEL => false
  | .ERROR => false

/-- Whitespace and comments, which trees sent to the checker leave out. -/
def Kind.isTrivia : Kind → Bool
  | .WHITESPACE => true
  | .LINE_COMMENT => true
  | .BLOCK_COMMENT => true
  | .IDENT => false
  | .QUOTED_IDENT => false
  | .VARIABLE => false
  | .PARAM => false
  | .NUMBER => false
  | .STRING => false
  | .DOLLAR_STRING => false
  | .DOLLAR_QUOTE => false
  | .OP => false
  | .SEMICOLON => false
  | .COMMA => false
  | .DOT => false
  | .COLON => false
  | .L_PAREN => false
  | .R_PAREN => false
  | .L_BRACKET => false
  | .R_BRACKET => false
  | .L_BRACE => false
  | .R_BRACE => false
  | .BATCH_SEP => false
  | .INFERRED_TERMINATOR => false
  | .ERROR_TOKEN => false
  | .SOURCE_FILE => false
  | .BATCH => false
  | .STATEMENT => false
  | .ROUTINE => false
  | .ROUTINE_BODY => false
  | .BLOCK => false
  | .DECLARE_SECTION => false
  | .EXCEPTION_SECTION => false
  | .EXCEPTION_HANDLER => false
  | .TRY_CATCH => false
  | .TRY_BLOCK => false
  | .CATCH_BLOCK => false
  | .IF_STMT => false
  | .LOOP_STMT => false
  | .CASE_STMT => false
  | .HANDLER => false
  | .CONDITION => false
  | .BRANCH => false
  | .LABEL => false
  | .ERROR => false

end Sqldiff.Syntax
