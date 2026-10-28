"""Protocol tests for the built checker. sqldiff-check/2: confirmed only
for identical sides that decode under the checker's syntax schema.
sqldiff-check/3: the C1 mirror check, confirmed for a T-SQL procedure and
its PL/pgSQL mirror, with the C2a fields (`scalars`, `residual`) and the
C3a field (`statements`) on a confirmed answer; C3a requests with table
mappings in DEPLOYMENT and table entries in CATALOG. Anything else is
unconfirmed, including malformed requests.

Usage: python3 test_gate.py PATH-TO-BINARY
"""

import json
import pathlib
import re
import subprocess
import sys

exe = sys.argv[1]
P = "sqldiff-check/2"
syntax = (pathlib.Path(__file__).parent / "Sqldiff" / "Syntax.lean").read_text(encoding="utf-8")
H = re.search(r'def schemaHash : String := "([0-9a-f]+)"', syntax).group(1)
# `SELECT 1` and `SELECT 2` as sqldiff_core::proof::tree_line writes them.
ONE = "(SOURCE_FILE (STATEMENT IDENT:73656c656374 NUMBER:31 INFERRED_TERMINATOR:))"
TWO = "(SOURCE_FILE (STATEMENT IDENT:73656c656374 NUMBER:32 INFERRED_TERMINATOR:))"


def req(i, xa, xb, ta="2022", db="tsql", schema=H):
    return f"{P}\t{i}\t{schema}\ttsql\t{ta}\t{xa}\t{db}\t2022\t{xb}\tend"


cases = [
    ("1", req(1, ONE, ONE), "confirmed"),
    ("2", req(2, ONE, TWO), "unconfirmed"),
    ("3", req(3, ONE, ONE, ta="2019"), "unconfirmed"),
    ("4", req(4, ONE, ONE, db="postgres"), "unconfirmed"),
    # Identical sides are confirmed only when they decode under this schema.
    ("5", req(5, ONE, ONE, schema="0000000000000000"), "unconfirmed"),
    ("6", req(6, "(ROOT)", "(ROOT)"), "unconfirmed"),
    ("7", req(7, "(IDENT)", "(IDENT)"), "unconfirmed"),
    ("8", req(8, "(SOURCE_FILE BATCH:31)", "(SOURCE_FILE BATCH:31)"), "unconfirmed"),
    ("9", req(9, "(SOURCE_FILE WHITESPACE:20)", "(SOURCE_FILE WHITESPACE:20)"), "unconfirmed"),
    ("10", req(10, "(SOURCE_FILE NUMBER:3)", "(SOURCE_FILE NUMBER:3)"), "unconfirmed"),
    ("11", req(11, "(SOURCE_FILE NUMBER:4A)", "(SOURCE_FILE NUMBER:4A)"), "unconfirmed"),
    ("12", req(12, ONE + " x", ONE + " x"), "unconfirmed"),
    ("13", req(13, "(SOURCE_FILE", "(SOURCE_FILE"), "unconfirmed"),
    ("0", req(14, ONE, ONE)[: -len("\tend")], "unconfirmed"),
    ("0", req("x15", ONE, ONE), "unconfirmed"),
    ("0", f"sqldiff-check/1\t16\ttsql\t2022\t{ONE}\ttsql\t2022\t{ONE}\tend", "unconfirmed"),
    ("0", "garbage", "unconfirmed"),
]

# sqldiff-check/3: the C1 mirror check on abstract syntax lines in the
# form `sqldiff_syntax::encode` writes (see Sqldiff/Ast.lean).
AST = re.search(
    r'def schemaHash : String := "([0-9a-f]+)"',
    (pathlib.Path(__file__).parent / "Sqldiff" / "Ast.lean").read_text(encoding="utf-8"),
).group(1)


def hx(t):
    return t.encode().hex()


def S(t):
    return "s:" + hx(t)


def C(tag, *kids):
    return "(" + " ".join([tag, *kids]) + ")"


def L(*xs):
    return "[" + " ".join(xs) + "]"


NONE = "-"
INT = C("Ty", S("int"), L())


def tv(x):
    return C("Var", S(x))


def pv(x):
    return C("Name", L(S(x)))


def num(n):
    return C("Num", S(str(n)))


def bin_(op, l, r):
    return C("Bin", S(op), l, r)


def fn(n, *args):
    return C("CallE", C("CallF", L(S(n)), L(*args), "f", L(), "f", "f", "f"))


def assign(x, v):
    return C("NStmt", C("Assign", x, v))


def param(n, mode, default=NONE):
    return C("Param", S(n), INT, C(mode), default)


def proc(name, params, decls, body):
    return C("Routine", C("Procedure"), L(*map(S, name)), L(*params), NONE, C("Caller"), NONE,
             C("Block", L(*decls), L(*body)))


def call(name, *args):
    return C("Call", L(*map(S, name)), L(*(C("Arg", NONE, e, o) for e, o in args)), NONE)


T_PARAMS = [param("@a", "In"), param("@b", "InOut")]
T_BODY = [
    C("NStmt", C("Declare", S("@x"), INT, num(1))),
    C("If", bin_(">", tv("@a"), num(1)), L(assign(tv("@x"), num(2))), L(assign(tv("@x"), num(3)))),
    C("While", bin_("<", tv("@x"), num(10)), L(assign(tv("@x"), bin_("+", tv("@x"), num(1))))),
    call(["dbo", "log_value"], (tv("@x"), "f"), (tv("@b"), "t")),
    C("NStmt", C("Return", tv("@x"))),
]
T_ROUTINE = proc(["dbo", "p"], T_PARAMS, [], T_BODY)


def p_routine(status):
    return proc(["p"], [param("p_a", "In"), param("p_b", "InOut"), param("return_code", "InOut", C("Null"))],
                [C("Declare", S("v_x"), INT, NONE)], [
        assign(pv("return_code"), num(0)),
        assign(pv("v_x"), num(1)),
        C("If", bin_(">", pv("p_a"), num(1)), L(assign(pv("v_x"), num(2))), L(assign(pv("v_x"), num(3)))),
        C("While", bin_("<", pv("v_x"), num(10)), L(assign(pv("v_x"), bin_("+", pv("v_x"), num(1))))),
        call(["log_value"], (pv("v_x"), "f"), (pv("p_b"), "f")),
        assign(pv("return_code"), status),
        C("NStmt", C("Return", NONE)),
    ])


P_MIRROR = p_routine(fn("COALESCE", pv("v_x"), num(0)))
P_TODAY = p_routine(pv("v_x"))
DEP = f"0,{hx('dbo.log_value')}:{hx('log_value')}"
CAT = f"{hx('log_value')}:ibb"
CERT = ",".join(f"{hx(a)}:{hx(b)}" for a, b in
                [("@a", "p_a"), ("@b", "p_b"), ("@x", "v_x"), ("%status", "return_code")])
P3 = "sqldiff-check/3"


def req3(i, a=T_ROUTINE, b=P_MIRROR, dep=DEP, cat=CAT, cert=CERT, schema=AST, kind="mirror"):
    return f"{P3}\t{i}\t{schema}\t{kind}\t{dep}\t{cat}\t{cert}\t{a}\t{b}\tend"


cases += [
    ("31", req3(31), "confirmed"),
    # What the converter emits today: `return_code := v_x`, no COALESCE.
    ("32", req3(32, b=P_TODAY), "unconfirmed"),
    ("33", req3(33, schema="0000000000000000"), "unconfirmed"),
    ("34", req3(34, cert="4078"), "unconfirmed"),
    ("35", req3(35, cat="-"), "unconfirmed"),
    # The callee is not in the deployment's routine map.
    ("36", req3(36, dep="0"), "unconfirmed"),
    # Row counts observed, but this routine has no SET NOCOUNT: still a mirror.
    ("40", req3(40, dep="1," + DEP[2:]), "confirmed"),
    ("37", req3(37, a=T_ROUTINE[:-1]), "unconfirmed"),
    ("38", req3(38, b=T_ROUTINE), "unconfirmed"),
    ("0", req3(39, kind="other"), "unconfirmed"),
]
# C2a: the same procedure under SET XACT_ABORT ON is in the scalar slice.
T_ABORT = proc(["dbo", "p"], T_PARAMS, [], [C("Setting", S("XACT_ABORT"), "t")] + T_BODY)
assert T_ABORT != T_ROUTINE


def pair_t(e):
    return proc(["r"], [param("@a", "In"), param("@r", "InOut")], [], [assign(tv("@r"), e)])


def pair_p(e):
    return proc(["r"], [param("a", "In"), param("r", "InOut")], [], [assign(pv("r"), e)])


PAIR_CERT = ",".join(f"{hx(a)}:{hx(b)}" for a, b in [("@a", "a"), ("@r", "r"), ("%status", "%status")])
cases += [
    ("41", req3(41, a=T_ABORT), "confirmed"),
    # T-SQL ISNULL(@a, 0) and PostgreSQL COALESCE(a, 0) on int: in the slice, nothing can raise.
    ("42", req3(42, a=pair_t(fn("ISNULL", tv("@a"), num(0))), b=pair_p(fn("COALESCE", pv("a"), num(0))),
                cat="-", dep="0", cert=PAIR_CERT), "confirmed"),
    # Integer division against `||`: refused.
    ("43", req3(43, a=pair_t(bin_("/", tv("@a"), num(2))), b=pair_p(bin_("||", pv("a"), num(2))),
                cat="-", dep="0", cert=PAIR_CERT), "unconfirmed"),
]

# C3a: data statements on a catalog table. T-SQL dbo.Acct (Id int PRIMARY
# KEY, Qty int NULL, Total int NOT NULL) is the target table acct (id, qty,
# total); the deployment maps the name, the catalog describes the table.


def upd(target, sets, where=NONE):
    return C("NStmt", C("Update", L(*map(S, target)), NONE, L(*(C("Set", L(S(c)), e) for c, e in sets)),
                        L(), where))


def ins(target, cols, rows):
    return C("NStmt", C("Insert", L(*map(S, target)), L(*map(S, cols)),
                        C("SrcValues", L(*(L(*r) for r in rows))), L(), L(), NONE))


def dele(target, where=NONE):
    return C("NStmt", C("Delete", L(*map(S, target)), NONE, L(), where))


def col(c):
    return C("Name", L(S(c)))


ABORT = C("Setting", S("XACT_ABORT"), "t")
D_T = proc(["dbo", "bump"], [param("@id", "In"), param("@n", "In")], [], [
    ABORT,
    upd(["dbo", "Acct"], [("Qty", bin_("+", col("Qty"), tv("@n")))], bin_("=", col("Id"), tv("@id"))),
    ins(["dbo", "Acct"], ["Id", "Qty", "Total"], [[tv("@id"), tv("@n"), num(0)]]),
])


def d_p(where=bin_("=", pv("id"), pv("p_id")), params=("p_id", "p_n")):
    return proc(["bump"], [param(params[0], "In"), param(params[1], "In")], [], [
        upd(["acct"], [("qty", bin_("+", pv("qty"), pv("p_n")))], where),
        ins(["acct"], ["id", "qty", "total"], [[pv(params[0]), pv("p_n"), num(0)]]),
    ])


D_DEP = f"0;{hx('dbo.Acct')}:{hx('acct')}"
D_CAT = (f"{hx('acct')}={hx('id')}.ink.{hx('Id')}/{hx('qty')}.i.{hx('Qty')}"
         f"/{hx('total')}.in.{hx('Total')}")
D_CERT = ",".join(f"{hx(a)}:{hx(b)}" for a, b in [("@id", "p_id"), ("@n", "p_n"), ("%status", "%status")])
D_ID_CERT = ",".join(f"{hx(a)}:{hx(b)}" for a, b in [("@id", "id"), ("@n", "p_n"), ("%status", "%status")])


def req3d(i, a=D_T, b=None, dep=D_DEP, cat=D_CAT, cert=D_CERT):
    return req3(i, a=a, b=d_p() if b is None else b, dep=dep, cat=cat, cert=cert)


cases += [
    ("44", req3d(44), "confirmed"),
    # The table missing from the catalog, or its name from the deployment.
    ("45", req3d(45, cat="-"), "unconfirmed"),
    ("46", req3d(46, dep="0"), "unconfirmed"),
    # A different WHERE.
    ("47", req3d(47, b=d_p(where=bin_(">=", pv("id"), pv("p_id")))), "unconfirmed"),
    # The PL/pgSQL parameter is called id, like the column: ambiguous.
    ("48", req3d(48, b=d_p(where=bin_("=", pv("id"), pv("id")), params=("id", "p_n")), cert=D_ID_CERT),
     "unconfirmed"),
    # Row counts observed: T-SQL sends one per statement, PostgreSQL none.
    ("49", req3d(49, dep="1" + D_DEP[1:]), "unconfirmed"),
    # A WHERE outside the scalar slice (IN): confirmed on the C1 theorem.
    ("50", req3(50, a=proc(["r"], [], [], [dele(["dbo", "Acct"], C("InList", col("Qty"), L(num(1), num(2)), "f"))]),
                b=proc(["r"], [], [], [dele(["acct"], C("InList", pv("qty"), L(num(1), num(2)), "f"))]),
                dep=D_DEP, cat=D_CAT, cert=f"{hx('%status')}:{hx('%status')}"), "confirmed"),
]
THEOREMS = {
    "31": "Sqldiff.Mirror.check_sound",
    "40": "Sqldiff.Mirror.check_sound",
    "41": "Sqldiff.Mirror.check_sound_c2a",
    "42": "Sqldiff.Mirror.check_sound_c2a",
    "44": "Sqldiff.Mirror.check_sound_c2a",
    "50": "Sqldiff.Mirror.check_sound",
}
# The fields a confirmed /3 answer adds (Main.lean); none on /2 or unconfirmed answers.
EXTRA = {
    "31": {"scalars": "uninterpreted", "statements": "c1"},
    "40": {"scalars": "uninterpreted", "statements": "c1"},
    "41": {"scalars": "c2a", "residual": ["ASM-TSQL-ARITH-ERRORS"], "statements": "c1"},
    "42": {"scalars": "c2a", "residual": [], "statements": "c1"},
    "44": {"scalars": "c2a",
           "residual": ["ASM-TSQL-ARITH-ERRORS", "ASM-TSQL-ANSI-NULLS", "ASM-STATE-AFTER-ERROR"],
           "statements": "c3a"},
    "50": {"scalars": "uninterpreted", "statements": "c3a"},
}
inp = "".join(line + "\n" for _, line, _ in cases)
out = subprocess.run([exe], input=inp.encode(), capture_output=True, check=True).stdout.decode()
answers = [json.loads(line) for line in out.splitlines()]
assert len(answers) == len(cases), out
for (want_id, line, want), got in zip(cases, answers):
    assert got["id"] == want_id, (line, got)
    assert got["proof"] == want, (line, got)
    if want == "confirmed":
        assert got["theorem"] == THEOREMS.get(want_id, "Sqldiff.Gate.check_sound"), got
        base = {"id", "proof", "theorem"}
    else:
        base = {"id", "proof", "reason"}
    extra = {k: v for k, v in got.items() if k not in base}
    assert extra == EXTRA.get(want_id, {}), got
print(f"gate protocol ok ({len(cases)} cases)")
