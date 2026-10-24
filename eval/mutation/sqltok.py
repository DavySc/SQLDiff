"""A small SQL tokenizer for the mutation harness.

It only needs to tell code from comments and string literals, for T-SQL and
PostgreSQL: `--` and (nested, in PostgreSQL) `/* */` comments, '...' strings
with '' escapes and N'' / E'' / U&'' prefixes, "..." and [...] quoted
identifiers, and PostgreSQL dollar quotes. A dollar-quoted routine body
(`AS $$ ... $$`, `DO $$ ... $$`) is code and is tokenized too; any other
dollar-quoted text is a string.
"""

import re

WORD = re.compile(r"[A-Za-z_\u0080-￿][A-Za-z0-9_$#\u0080-￿]*")
NUM = re.compile(r"(\d+\.\d*|\.\d+|\d+)([eE][+-]?\d+)?")
OPS = ["<>", "!=", "<=", ">=", "!<", "!>", "::", ":=", "||", "+=", "-=", "*=", "/=", "%=", "=>", "->>", "->", "#>>", "#>", "@>", "<@", "&&"]
DOLLAR = re.compile(r"\$([A-Za-z_][A-Za-z0-9_]*)?\$")


class Tok:
    __slots__ = ("kind", "text", "start", "end", "line")

    def __init__(self, kind, text, start, line):
        self.kind = kind  # ws comment string qident word num op punct var body_open body_close
        self.text = text
        self.start = start
        self.end = start + len(text)
        self.line = line

    def __repr__(self):
        return f"{self.kind}:{self.text!r}@{self.line}"

    @property
    def up(self):
        return self.text.upper()

    @property
    def code(self):
        return self.kind not in ("ws", "comment", "string")


def tokenize(src, dialect):
    toks = []
    i = 0
    n = len(src)
    line = 1
    pg = dialect == "postgres"
    body_stack = []  # open dollar tags of routine bodies

    def add(kind, j):
        nonlocal i, line
        text = src[i:j]
        toks.append(Tok(kind, text, i, line))
        line += text.count("\n")
        i = j

    def prev_code_word():
        for t in reversed(toks):
            if t.kind in ("ws", "comment"):
                continue
            return t.up if t.kind == "word" else None
        return None

    while i < n:
        c = src[i]
        if c in " \t\r\n\f﻿":
            j = i
            while j < n and src[j] in " \t\r\n\f﻿":
                j += 1
            add("ws", j)
        elif src.startswith("--", i):
            j = src.find("\n", i)
            add("comment", n if j < 0 else j)
        elif src.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if src.startswith("/*", j):
                    depth += 1 if pg else 0
                    j += 2
                elif src.startswith("*/", j):
                    depth -= 1
                    j += 2
                else:
                    j += 1
            add("comment", j)
        elif c == "'" or (c in "NnEeBbXx" and src[i + 1:i + 2] == "'") or (src[i:i + 3].upper() == "U&'"):
            j = src.index("'", i) + 1
            backslash = pg and c in "Ee"
            while j < n:
                if backslash and src[j] == "\\":
                    j += 2
                    continue
                if src[j] == "'":
                    if src[j + 1:j + 2] == "'":
                        j += 2
                        continue
                    j += 1
                    break
                j += 1
            add("string", j)
        elif c == '"':
            j = i + 1
            while j < n:
                if src[j] == '"':
                    if src[j + 1:j + 2] == '"':
                        j += 2
                        continue
                    j += 1
                    break
                j += 1
            add("qident", j)
        elif c == "[" and not pg:
            j = src.find("]", i)
            add("qident", n if j < 0 else j + 1)
        elif c == "$" and pg and DOLLAR.match(src, i):
            m = DOLLAR.match(src, i)
            tag = m.group(0)
            if body_stack and body_stack[-1] == tag:
                body_stack.pop()
                add("body_close", m.end())
            elif prev_code_word() in ("AS", "DO") and not body_stack:
                body_stack.append(tag)
                add("body_open", m.end())
            else:
                j = src.find(tag, m.end())
                add("string", n if j < 0 else j + len(tag))
        elif c == "@" and not pg:
            j = i + 1
            while j < n and (src[j].isalnum() or src[j] in "_@#$"):
                j += 1
            add("var" if j > i + 1 else "op", j)
        elif c.isdigit() or (c == "." and src[i + 1:i + 2].isdigit()):
            m = NUM.match(src, i)
            add("num", m.end())
        elif WORD.match(src, i):
            add("word", WORD.match(src, i).end())
        else:
            for op in OPS:
                if src.startswith(op, i):
                    add("op", i + len(op))
                    break
            else:
                add("punct" if c in "(),;." else "op", i + 1)
    return toks


def code_tokens(toks):
    return [t for t in toks if t.code]
