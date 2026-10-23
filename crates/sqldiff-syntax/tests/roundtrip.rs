//! print(parse(x)) == x for arbitrary input, in every dialect, without panics.

use sqldiff_syntax::{lexer, parse, Dialect};

const PIECES: &[&str] = &[
    " ",
    "\n",
    "\t",
    "\r\n",
    "--c\n",
    "/*",
    "*/",
    "'",
    "''",
    "\"",
    "[",
    "]",
    "]]",
    "(",
    ")",
    ";",
    ",",
    ".",
    ":",
    "::",
    ":=",
    "$",
    "$$",
    "$a$",
    "$1",
    "@",
    "@@",
    "#",
    "?",
    "<<",
    ">>",
    "->>",
    "+",
    "-",
    "*",
    "/",
    "=",
    "<>",
    "N'",
    "E'",
    "U&'",
    "X'",
    "\\",
    "0x1F",
    "1.5e3",
    ".5",
    "é",
    "ü",
    "SELECT",
    "BEGIN",
    "END",
    "IF",
    "THEN",
    "ELSE",
    "ELSIF",
    "CASE",
    "WHEN",
    "LOOP",
    "WHILE",
    "DO",
    "FOR",
    "REPEAT",
    "UNTIL",
    "DECLARE",
    "HANDLER",
    "EXIT",
    "TRY",
    "CATCH",
    "TRAN",
    "GO",
    "CREATE",
    "PROCEDURE",
    "FUNCTION",
    "AS",
    "LANGUAGE",
    "plpgsql",
    "ATOMIC",
    "EXCEPTION",
    "RETURN",
    "x",
    "lbl",
    "INSERT",
    "UPDATE",
    "SET",
    "THROW",
    "ROLLBACK",
];

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

#[test]
fn random_inputs_round_trip() {
    let mut rng = Lcg(0x5eed);
    for _ in 0..3000 {
        let n = (rng.next() % 40) as usize;
        let text: String = (0..n)
            .map(|_| PIECES[(rng.next() as usize) % PIECES.len()])
            .collect();
        for d in Dialect::ALL {
            let lexed = lexer::tokenize(&text, d);
            let joined: String = lexed.tokens.iter().map(|t| &text[t.start..t.end]).collect();
            assert_eq!(joined, text, "lexer lost bytes for {d}: {text:?}");
            let p = parse(&text, d);
            assert_eq!(
                p.syntax().text().to_string(),
                text,
                "parser lost bytes for {d}: {text:?}"
            );
        }
    }
}

#[test]
fn fixtures_round_trip_cleanly() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures");
    let mut count = 0;
    for entry in std::fs::read_dir(dir).expect("fixtures dir") {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let dialect = if name.ends_with(".tsql.sql") {
            Dialect::Tsql
        } else if name.ends_with(".pg.sql") {
            Dialect::Postgres
        } else if name.ends_with(".db2i.sql") {
            Dialect::Db2i
        } else {
            continue;
        };
        let text = std::fs::read_to_string(&path).unwrap();
        let p = parse(&text, dialect);
        assert_eq!(p.syntax().text().to_string(), text, "{name}");
        assert!(p.diagnostics.is_empty(), "{name}: {:?}", p.diagnostics);
        count += 1;
    }
    assert!(count > 0);
}
