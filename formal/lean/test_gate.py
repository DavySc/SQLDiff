"""Protocol tests for the built checker. sqldiff-check/2: confirmed only
for identical sides that decode under the checker's syntax schema.
sqldiff-check/3: the C1 mirror check, confirmed for a T-SQL procedure and
its PL/pgSQL mirror, with the C2a fields (`scalars`, `residual`) and the
C3a field (`statements`) on a confirmed answer; C3a requests with table
mappings in DEPLOYMENT and table entries in CATALOG; C2b string pairs,
with the collation fact in CATALOG and the string residuals; C4 pairs
(TRY/CATCH against EXCEPTION blocks, THROW against RAISE, the
rollback-and-rethrow idiom with COMMIT), with the C4 residuals and the
`errors` field; C3b queries (`SELECT @x = col` against the PL/pgSQL `FOR`
block, `EXISTS`, scalar subqueries, `COUNT(*)`), with `statements` `c3b`;
C3c string columns and constant defaults in the CATALOG; (0.13.0) the
`why` reason codes; (0.14.0) C2c typed values (integer widths, bit against
boolean, date and datetime2 with DATEADD/DATEDIFF, typed columns in the
CATALOG); (0.16.0) C3d query blocks (joins, aliases, qualified names, the
key closure, a scalar subquery over a join); the `why` reason codes on an unconfirmed answer whose routine is outside the
spec (Sqldiff/Mirror/Diagnose.lean), and their absence on every other
answer, confirmed or not.
Anything else is unconfirmed, including malformed requests.

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
# (C2c) The callee's parameter types, `/`-separated, one per mode letter.
CAT = f"{hx('log_value')}:ibb:" + "/".join([hx("INTEGER")] * 3)
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

# C2b: strings. T-SQL @a, @b varchar(10) and @r varchar(20) OUTPUT; the
# PL/pgSQL mirror takes text. `!binary-collation` in CATALOG is the
# collation fact (Sqldiff.Mirror.Catalog.binaryCollation).


def ty(name, *args):
    return C("Ty", S(name), L(*map(S, args)))


def sparam(n, mode, t):
    return C("Param", S(n), t, C(mode), NONE)


def string(t):
    return C("Str", S(t))


VC10, VC3, VC20, TEXT = ty("VARCHAR", "10"), ty("VARCHAR", "3"), ty("VARCHAR", "20"), ty("TEXT")


def s_t(body, ta=VC10):
    return proc(["r"], [sparam("@a", "In", ta), sparam("@b", "In", ta), sparam("@r", "InOut", VC20)], [], body)


def s_p(body, rt=TEXT):
    return proc(["r"], [sparam("a", "In", TEXT), sparam("b", "In", TEXT), sparam("r", "InOut", rt)], [], body)


S_CERT = ",".join(f"{hx(a)}:{hx(b)}" for a, b in [("@a", "a"), ("@b", "b"), ("@r", "r"), ("%status", "%status")])


def req3s(i, a, b, cat="-"):
    return req3(i, a=a, b=b, dep="0", cat=cat, cert=S_CERT)


def if_eq(cond, set_):
    return C("If", cond, L(set_(string("eq"))), L(set_(string("ne"))))


EQ_T = s_t([if_eq(bin_("=", tv("@a"), tv("@b")), lambda v: assign(tv("@r"), v))])
EQ_P_RTRIM = s_p([if_eq(bin_("=", fn("rtrim", pv("a")), fn("rtrim", pv("b"))), lambda v: assign(pv("r"), v))])
EQ_P_PLAIN = s_p([if_eq(bin_("=", pv("a"), pv("b")), lambda v: assign(pv("r"), v))])
LEN_T = proc(["r"], [sparam("@a", "In", VC10), param("@r", "InOut")], [],
             [assign(tv("@r"), fn("LEN", tv("@a")))])


def len_p(e):
    return proc(["r"], [sparam("a", "In", TEXT), param("r", "InOut")], [], [assign(pv("r"), e)])


LEN_CERT = ",".join(f"{hx(a)}:{hx(b)}" for a, b in [("@a", "a"), ("@r", "r"), ("%status", "%status")])
cases += [
    # `@r = @a + @b` against `r := a || b`.
    ("51", req3s(51, s_t([assign(tv("@r"), bin_("+", tv("@a"), tv("@b")))]),
                 s_p([assign(pv("r"), bin_("||", pv("a"), pv("b")))])), "confirmed"),
    # T-SQL `=` pads with spaces: it is `rtrim(a) = rtrim(b)`, given the collation fact.
    ("52", req3s(52, EQ_T, EQ_P_RTRIM, cat="!binary-collation"), "confirmed"),
    # Without the fact string `=` is an uninterpreted operator: the rtrim form differs.
    ("53", req3s(53, EQ_T, EQ_P_RTRIM), "unconfirmed"),
    # Plain `a = b` counts trailing spaces: refused.
    ("54", req3s(54, EQ_T, EQ_P_PLAIN, cat="!binary-collation"), "unconfirmed"),
    # An unknown fact does not decode.
    ("55", req3s(55, EQ_T, EQ_P_RTRIM, cat="!case-insensitive"), "unconfirmed"),
    # LEN(@a) against length(rtrim(a)), and against length(a).
    ("56", req3(56, a=LEN_T, b=len_p(fn("length", fn("rtrim", pv("a")))), dep="0", cat="-", cert=LEN_CERT),
     "confirmed"),
    ("57", req3(57, a=LEN_T, b=len_p(fn("length", pv("a"))), dep="0", cat="-", cert=LEN_CERT), "unconfirmed"),
    # ISNULL(@a, 'abcdef') with @a varchar(3) truncates; COALESCE does not.
    ("58", req3s(58, s_t([assign(tv("@r"), fn("ISNULL", tv("@a"), string("abcdef")))], ta=VC3),
                 s_p([assign(pv("r"), fn("COALESCE", pv("a"), string("abcdef")))])), "unconfirmed"),
    # 'none' fits @a varchar(10): ISNULL is COALESCE.
    ("59", req3s(59, s_t([assign(tv("@r"), fn("ISNULL", tv("@a"), string("none")))]),
                 s_p([assign(pv("r"), fn("COALESCE", pv("a"), string("none")))])), "confirmed"),
]

# C4: errors and transactions, in the shapes `sqldiff convert` writes.
# T-SQL TRY/CATCH is (Try body [(Handler [] catch)]); a PL/pgSQL block
# with EXCEPTION WHEN OTHERS is (Try body [(Handler [s:others] handler)]).


def try_(body, handler, conds=()):
    return C("Try", L(*body), L(C("Handler", L(*map(S, conds)), L(*handler))))


NOCOUNT = C("Setting", S("NOCOUNT"), "t")
RETHROW = C("Throw", L())
RERAISE = C("Raise", NONE, NONE, L(), L())


def throw(n, m, s=1):
    return C("Throw", L(num(n), string(m), num(s)))


def raise_(m, code="P0001", level="EXCEPTION"):
    return C("Raise", S(level), NONE, L(),
             L(C("RaiseOpt", S("MESSAGE"), string(m)), C("RaiseOpt", S("ERRCODE"), string(code))))


def txn(op):
    return C("Txn", S(op), NONE)


ROLLBACK_IF_OPEN = C("If", bin_(">", tv("@@TRANCOUNT"), num(0)), L(txn("ROLLBACK")), L())
AB_CERT = ",".join(f"{hx(a)}:{hx(b)}" for a, b in [("@a", "p_a"), ("@b", "p_b"), ("%status", "%status")])
ID_CERT = ",".join(f"{hx(a)}:{hx(b)}" for a, b in [("@id", "p_id"), ("%status", "%status")])
C4_P1_T = proc(["dbo", "p1"], [param("@a", "In"), param("@b", "InOut")], [], [
    ABORT, NOCOUNT,
    try_([assign(tv("@b"), bin_("/", tv("@a"), num(0)))], [assign(tv("@b"), C("Num", S("-1")))])])
C4_P1_P = proc(["p1"], [param("p_a", "In"), param("p_b", "InOut")], [], [
    try_([assign(pv("p_b"), bin_("/", pv("p_a"), num(0)))], [assign(pv("p_b"), C("Num", S("-1")))],
         ["others"])])
INS_T = ins(["dbo", "Acct"], ["Id", "Qty", "Total"], [[tv("@id"), num(1), num(0)]])
INS_P = ins(["acct"], ["id", "qty", "total"], [[pv("p_id"), num(1), num(0)]])
BUMP_T = upd(["dbo", "Acct"], [("Total", bin_("+", col("Total"), num(1)))])
BUMP_P = upd(["acct"], [("total", bin_("+", pv("total"), num(1)))])


def id_t(body):
    return proc(["dbo", "p"], [param("@id", "In")], [], [ABORT, NOCOUNT] + body)


def id_p(body):
    return proc(["p"], [param("p_id", "In")], [], body)


def req3e(i, a, b, cert=ID_CERT):
    return req3(i, a=a, b=b, dep=D_DEP, cat=D_CAT, cert=cert)


cases += [
    # A handled division by zero: the TRY body writes nothing.
    ("60", req3e(60, C4_P1_T, C4_P1_P, cert=AB_CERT), "confirmed"),
    # A duplicate key re-raised as the routine's own error (THROW / RAISE EXCEPTION).
    ("61", req3e(61, id_t([try_([INS_T], [throw(50001, "dup")])]),
                 id_p([try_([INS_P], [raise_("dup")], ["others"])])), "confirmed"),
    # CATCH THROW; after two writes: the caller sees a failed call either way.
    ("62", req3e(62, id_t([try_([INS_T, BUMP_T], [RETHROW])]),
                 id_p([try_([INS_P, BUMP_P], [RERAISE], ["others"])])), "confirmed"),
    # The handler goes on after a write the block rolls back: refused.
    ("63", req3e(63, id_t([try_([INS_T, BUMP_T], [assign(tv("@id"), num(0))])]),
                 id_p([try_([INS_P, BUMP_P], [assign(pv("p_id"), num(0))], ["others"])])), "unconfirmed"),
    # The rollback-and-rethrow idiom: the converter keeps the body and the COMMIT.
    ("64", req3e(64, id_t([try_([txn("BEGIN"), INS_T, BUMP_T, txn("COMMIT")], [ROLLBACK_IF_OPEN, RETHROW])]),
                 id_p([INS_P, BUMP_P, txn("COMMIT")])), "confirmed"),
    # ERROR_NUMBER() against SQLSTATE: refused.
    ("65", req3e(65, id_t([try_([INS_T], [assign(tv("@id"), fn("ERROR_NUMBER"))])]),
                 id_p([try_([INS_P], [assign(pv("p_id"), pv("SQLSTATE"))], ["others"])])), "unconfirmed"),
    # RAISE NOTICE is a message, not an error.
    ("66", req3e(66, id_t([try_([INS_T], [throw(50001, "dup")])]),
                 id_p([try_([INS_P], [raise_("dup", level="NOTICE")], ["others"])])), "unconfirmed"),
]

# Soundness fixes (checker 0.8.0).


def func(name, params, ret, body):
    return C("Routine", C("Function"), L(*map(S, name)), L(*params), ret, C("Caller"), NONE,
             C("Block", L(), L(*body)))


def dparam(n, t, default):
    return C("Param", S(n), t, C("In"), default)


R_CERT = ",".join(f"{hx(a)}:{hx(b)}" for a, b in [("@r", "r"), ("@s", "s"), ("%status", "%status")])


def callee_t(pre):
    return proc(["dbo", "p"], [param("@a", "In"), param("@r", "InOut")], [],
                pre + [assign(tv("@r"), num(0)), call(["dbo", "log_value"], (tv("@a"), "f")),
                       assign(tv("@r"), num(1))])


CALLEE_P = proc(["p"], [param("a", "In"), param("r", "InOut")], [],
                [assign(pv("r"), num(0)), call(["log_value"], (pv("a"), "f")), assign(pv("r"), num(1))])


def two_brackets(handler):
    return id_t([try_([txn("BEGIN"), INS_T, txn("COMMIT"), assign(tv("@id"), bin_("/", num(10), tv("@id"))),
                       txn("BEGIN"), BUMP_T, txn("COMMIT")], handler)])


TWO_BRACKETS_P = id_p([INS_P, txn("COMMIT"), assign(pv("p_id"), bin_("/", num(10), pv("p_id"))),
                       BUMP_P, txn("COMMIT")])
cases += [
    # Parameter and return types are compared: varchar(10) against integer.
    ("67", req3(67, a=func(["dbo", "f"], [sparam("@a", "In", VC10)], VC10, [C("NStmt", C("Return", tv("@a")))]),
                b=func(["f"], [sparam("a", "In", INT)], INT, [C("NStmt", C("Return", pv("a")))]),
                dep="0", cat="-", cert=f"{hx('@a')}:{hx('a')}"), "unconfirmed"),
    # A parameter default T-SQL truncates ('abcdef' into varchar(3)) and PostgreSQL keeps.
    ("68", req3(68, a=proc(["dbo", "pd"], [sparam("@r", "InOut", VC10), dparam("@s", VC3, string("abcdef"))], [],
                           [assign(tv("@r"), tv("@s"))]),
                b=proc(["pd"], [sparam("r", "InOut", VC10), dparam("s", VC3, string("abcdef"))], [],
                       [assign(pv("r"), pv("s"))]),
                dep="0", cat="-", cert=R_CERT), "unconfirmed"),
    # A default that fits is in the slice (r is text: a PostgreSQL parameter
    # is read without its length, so s fits only an unbounded target).
    ("69", req3(69, a=proc(["dbo", "pd"], [sparam("@r", "InOut", VC10), dparam("@s", VC3, string("abc"))], [],
                           [assign(tv("@r"), tv("@s"))]),
                b=proc(["pd"], [sparam("r", "InOut", TEXT), dparam("s", VC3, string("abc"))], [],
                       [assign(pv("r"), pv("s"))]),
                dep="0", cat="-", cert=R_CERT), "confirmed"),
    # A call without XACT_ABORT ON: the callee's error lets T-SQL go on and
    # ends the PostgreSQL CALL, so not a C2a answer.
    ("70", req3(70, a=callee_t([]), b=CALLEE_P, cert=PAIR_CERT), "confirmed"),
    ("71", req3(71, a=callee_t([ABORT]), b=CALLEE_P, cert=PAIR_CERT), "confirmed"),
    # ERRCODE = 'p0001' is not P0001 (PostgreSQL: 42704).
    ("72", req3e(72, id_t([try_([INS_T], [throw(50001, "dup")])]),
                 id_p([try_([INS_P], [raise_("dup", code="p0001")], ["others"])])), "unconfirmed"),
    # Unguarded ROLLBACK; THROW; around two brackets: an error between them
    # finds no transaction open (3903).
    ("73", req3e(73, two_brackets([txn("ROLLBACK"), RETHROW]), TWO_BRACKETS_P), "unconfirmed"),
    # The guarded form is fine.
    ("74", req3e(74, two_brackets([ROLLBACK_IF_OPEN, RETHROW]), TWO_BRACKETS_P), "confirmed"),
    # The idiom inside an IF: no transaction statements there.
    ("75", req3e(75, id_t([C("If", bin_(">", tv("@id"), num(0)),
                             L(try_([txn("BEGIN"), INS_T, txn("COMMIT")], [ROLLBACK_IF_OPEN, RETHROW])), L())]),
                 id_p([C("If", bin_(">", pv("p_id"), num(0)), L(INS_P, txn("COMMIT")), L())])), "unconfirmed"),
]

# String stores on every path (checker 0.9.0): a store into a target that
# may hold a string must provably keep the value.
LONG = string("abcdefghijklmnopqrstuvwxyz")
VC3_DECL_T = C("NStmt", C("Declare", S("@x"), VC3, string("abcdef")))
X_CERT = ",".join(f"{hx(a)}:{hx(b)}" for a, b in
                  [("@a", "a"), ("@b", "b"), ("@r", "r"), ("@x", "x"), ("%status", "%status")])
cases += [
    # SET @r = <26 characters> into varchar(20): T-SQL truncates.
    ("76", req3s(76, s_t([assign(tv("@r"), LONG)]), s_p([assign(pv("r"), LONG)])), "unconfirmed"),
    # SET @r = UPPER(@a): a value outside the slice into varchar(20).
    ("77", req3s(77, s_t([assign(tv("@r"), fn("UPPER", tv("@a")))]), s_p([assign(pv("r"), fn("upper", pv("a")))])),
     "unconfirmed"),
    # DECLARE @x varchar(3) = 'abcdef' against x varchar(3) := 'abcdef'.
    ("78", req3(78, a=s_t([VC3_DECL_T, assign(tv("@r"), tv("@x"))]),
                b=proc(["r"], [sparam("a", "In", TEXT), sparam("b", "In", TEXT), sparam("r", "InOut", TEXT)],
                       [C("Declare", S("x"), VC3, string("abcdef"))], [assign(pv("r"), pv("x"))]),
                dep="0", cat="-", cert=X_CERT), "unconfirmed"),
    # A function RETURNS varchar(5) returning a 6-character value.
    ("79", req3(79, a=func(["dbo", "f"], [sparam("@a", "In", ty("VARCHAR", "5"))], ty("VARCHAR", "5"),
                           [C("NStmt", C("Return", bin_("+", tv("@a"), string("x"))))]),
                b=func(["f"], [sparam("a", "In", ty("VARCHAR", "5"))], TEXT,
                       [C("NStmt", C("Return", bin_("||", pv("a"), string("x"))))]),
                dep="0", cat="-", cert=f"{hx('@a')}:{hx('a')}"), "unconfirmed"),
    # OUTPUT copy-back into @r varchar(20): the callee's value has no bound.
    ("80", req3(80, a=s_t([call(["dbo", "log_value"], (tv("@a"), "f"), (tv("@r"), "t"))]),
                b=s_p([call(["log_value"], (pv("a"), "f"), (pv("r"), "f"))]),
                cat=f"{hx('log_value')}:ibb:" + "/".join([hx("TEXT")] * 3), cert=S_CERT), "unconfirmed"),
    # The PostgreSQL side alone: x varchar(3) := 'abcdef' raises (a local
    # keeps its length), where T-SQL's @x varchar(10) holds it.
    ("81", req3(81, a=s_t([C("NStmt", C("Declare", S("@x"), VC10, string("abcdef"))), assign(tv("@r"), tv("@x"))]),
                b=proc(["r"], [sparam("a", "In", TEXT), sparam("b", "In", TEXT), sparam("r", "InOut", TEXT)],
                       [C("Declare", S("x"), VC3, string("abcdef"))], [assign(pv("r"), pv("x"))]),
                dep="0", cat="-", cert=X_CERT), "unconfirmed"),
    # What the converter writes for SET @r = @a with both varchar(10): the
    # PostgreSQL parameters keep no length, so the copy is kept (C1 answer).
    ("82", req3s(82, s_t([assign(tv("@r"), tv("@a"))]),
                 proc(["r"], [sparam("a", "In", VC10), sparam("b", "In", VC10), sparam("r", "InOut", VC20)], [],
                      [assign(pv("r"), pv("a"))])), "confirmed"),
]

# C3b: single-table queries, in the shapes `sqldiff convert` writes, on the
# C3a table: T-SQL SELECT @q = Qty against the PL/pgSQL FOR block, IF
# [NOT] EXISTS, a scalar subquery, COUNT(*).


def sel(items, target, where=NONE, into=()):
    return C("Select", L(), "f", L(), "f", NONE, L(*items), L(*into), "f",
             L(C("Table", L(*map(S, target)), NONE, L())), where, L(), NONE, L(), NONE, NONE, L(), NONE)


def item(e, alias=NONE, assign_=NONE):
    return C("SelItem", e, alias, assign_)


def for_block(items, target, where, body, r="sqldiff_r"):
    return C("NBlock", C("Block", L(C("Declare", S(r), C("Ty", S("RECORD"), L()), NONE)),
                         L(C("ForQuery", S(r), sel(items, target, where), L(*body)))))


def field(x, a, r="sqldiff_r"):
    return assign(pv(x), C("Name", L(S(r), S(a))))


COUNT_STAR = C("CallE", C("CallF", L(S("COUNT")), L(), "f", L(), "f", "f", "t"))
Q_KEY_T = bin_("=", col("Id"), tv("@id"))
Q_KEY_P = bin_("=", pv("id"), pv("p_id"))
Q_CERT = ",".join(f"{hx(a)}:{hx(b)}" for a, b in [("@id", "p_id"), ("@q", "p_q"), ("%status", "%status")])


def q_t(body):
    return proc(["dbo", "look"], [param("@id", "In"), param("@q", "InOut")], [], body)


def q_p(body):
    return proc(["look"], [param("p_id", "In"), param("p_q", "InOut")], [], body)


def req3q(i, a, b):
    return req3(i, a=a, b=b, dep=D_DEP, cat=D_CAT, cert=Q_CERT)


def exists_(target, where):
    return C("Exists", sel([item(num(1))], target, where))


SEL_T = C("NStmt", C("SSelect", sel([item(col("Qty"), assign_=S("@q"))], ["dbo", "Acct"], Q_KEY_T)))
FOR_P = for_block([item(pv("qty"), alias=S("sqldiff_c1"))], ["acct"], Q_KEY_P, [field("p_q", "sqldiff_c1")])
SCALAR_T = assign(tv("@q"), C("Subquery", sel([item(col("Total"))], ["dbo", "Acct"], Q_KEY_T)))
SCALAR_P = assign(pv("p_q"), C("Subquery", sel([item(pv("total"))], ["acct"], Q_KEY_P)))
EX_T = exists_(["dbo", "Acct"], bin_(">", col("Qty"), tv("@id")))
EX_P = exists_(["acct"], bin_(">", pv("qty"), pv("p_id")))
cases += [
    ("83", req3q(83, q_t([SEL_T]), q_p([FOR_P])), "confirmed"),
    # The WHERE fixes no key: which row's value T-SQL keeps is unspecified.
    ("84", req3q(84, q_t([C("NStmt", C("SSelect", sel([item(col("Qty"), assign_=S("@q"))], ["dbo", "Acct"],
                                                      bin_("=", col("Qty"), tv("@id")))))]),
                 q_p([for_block([item(pv("qty"), alias=S("sqldiff_c1"))], ["acct"], bin_("=", pv("qty"), pv("p_id")),
                                [field("p_q", "sqldiff_c1")])])), "unconfirmed"),
    # IF EXISTS against IF NOT EXISTS with the branches kept.
    ("85", req3q(85, q_t([C("If", EX_T, L(assign(tv("@q"), num(1))), L())]),
                 q_p([C("If", C("Unary", S("NOT"), EX_P), L(assign(pv("p_q"), num(1))), L())])), "unconfirmed"),
    ("86", req3q(86, q_t([C("If", EX_T, L(assign(tv("@q"), num(1))), L())]),
                 q_p([C("If", EX_P, L(assign(pv("p_q"), num(1))), L())])), "confirmed"),
    ("87", req3q(87, q_t([SCALAR_T]), q_p([SCALAR_P])), "confirmed"),
    # SELECT @q = ... keeps @q on no row; the scalar subquery sets NULL.
    ("88", req3q(88, q_t([SEL_T]), q_p([assign(pv("p_q"), C("Subquery", sel([item(pv("qty"))], ["acct"], Q_KEY_P)))])),
     "unconfirmed"),
    ("89", req3q(89, q_t([ABORT, C("NStmt", C("SSelect", sel([item(COUNT_STAR, assign_=S("@q"))], ["dbo", "Acct"])))]),
                 q_p([C("NStmt", C("SSelect", sel([item(COUNT_STAR)], ["acct"], into=[pv("p_q")])))])), "confirmed"),
    # The FOR loop's body reads a field the select list does not alias.
    ("90", req3q(90, q_t([SEL_T]), q_p([for_block([item(pv("qty"), alias=S("sqldiff_c1"))], ["acct"], Q_KEY_P,
                                                  [field("p_q", "sqldiff_c2")])])), "unconfirmed"),
]

# C3c: wider tables. T-SQL dbo.Cust (Id int PRIMARY KEY, Name varchar(20) NOT
# NULL, City nvarchar(30) NULL DEFAULT 'Paris', Qty int NOT NULL DEFAULT 0)
# is the target table cust; a string column is `v<n>`/`w<n>` in its SPEC, a
# default the fourth field (`s<hex>`, `i<int>`).
C_DEP = f"0;{hx('dbo.Cust')}:{hx('cust')}"


def c_cat(name="v20n", city=f"w30.{hx('City')}.s{hx('Paris')}"):
    return (f"{hx('cust')}={hx('id')}.ink.{hx('Id')}/{hx('name')}.{name}.{hx('Name')}"
            f"/{hx('city')}.{city}/{hx('qty')}.in.{hx('Qty')}.i0")


C_CERT = ",".join(f"{hx(a)}:{hx(b)}" for a, b in
                  [("@id", "p_id"), ("@c", "p_c"), ("@n", "v_n"), ("%status", "%status")])
VC30, NVC30 = ty("VARCHAR", "30"), ty("NVARCHAR", "30")
C_KEY_T = bin_("=", col("Id"), tv("@id"))
C_KEY_P = bin_("=", pv("Id"), pv("p_id"))


def c_t(name_value=tv("@n")):
    return proc(["dbo", "addc"], [param("@id", "In"), sparam("@c", "InOut", NVC30)], [], [
        ABORT,
        C("NStmt", C("Declare", S("@n"), VC20, string("abc"))),
        ins(["dbo", "Cust"], ["Id", "Name"], [[tv("@id"), name_value]]),
        upd(["dbo", "Cust"], [("Name", fn("RTRIM", col("Name"))), ("City", string("Lyon"))], C_KEY_T),
        C("NStmt", C("SSelect", sel([item(col("City"), assign_=S("@c"))], ["dbo", "Cust"], C_KEY_T))),
    ])


def c_p(local=VC20):
    return proc(["addc"], [param("p_id", "In"), sparam("p_c", "InOut", VC30)],
                [C("Declare", S("v_n"), local, NONE)], [
        assign(pv("v_n"), string("abc")),
        ins(["Cust"], ["Id", "Name"], [[pv("p_id"), pv("v_n")]]),
        upd(["Cust"], [("Name", fn("rtrim", pv("Name"))), ("City", string("Lyon"))], C_KEY_P),
        for_block([item(pv("City"), alias=S("sqldiff_c1"))], ["Cust"], C_KEY_P, [field("p_c", "sqldiff_c1")]),
    ])


def req3c(i, a=None, b=None, cat=None):
    return req3(i, a=c_t() if a is None else a, b=c_p() if b is None else b, dep=C_DEP,
                cat=c_cat() if cat is None else cat, cert=C_CERT)


cases += [
    ("91", req3c(91), "confirmed"),
    # Name as an int column: a string is not stored into it.
    ("92", req3c(92, cat=c_cat(name="in")), "unconfirmed"),
    # A default that does not fit its column: the table is not usable.
    ("93", req3c(93, cat=c_cat(city=f"w3.{hx('City')}.s{hx('Paris')}")), "unconfirmed"),
    # A string key column: the table is not usable.
    ("94", req3c(94, cat=c_cat(name="v20k")), "unconfirmed"),
    # 21 characters into varchar(20): 2628 against 22001.
    ("95", req3c(95, a=c_t(string("abcdefghijklmnopqrstu"))), "unconfirmed"),
    # The PL/pgSQL local v_n as varchar(21): its value may not fit Name
    # (varchar(20)), so the INSERT is outside the spec.
    ("96", req3c(96, b=c_p(local=ty("VARCHAR", "21"))), "unconfirmed"),
    # A malformed SPEC or DEFAULT: the catalog does not decode.
    ("97", req3c(97, cat=c_cat(name="v")), "unconfirmed"),
    ("98", req3c(98, cat=c_cat(city=f"w30.{hx('City')}.x1")), "unconfirmed"),
]
# 0.13.0: refusal diagnostics. One routine per family; the unconfirmed
# cases above whose reason is "outside the C1 spec" carry `why` too (EXTRA).
DEC = ty("DECIMAL", "18", "2")
cases += [
    # A result set: SELECT Qty FROM dbo.Acct WHERE Id = @id assigns nothing.
    ("99", req3q(99, q_t([C("NStmt", C("SSelect", sel([item(col("Qty"))], ["dbo", "Acct"], Q_KEY_T)))]),
                 q_p([FOR_P])), "unconfirmed"),
    # A setting the rules do not know, and @@ROWCOUNT: both named, in order.
    ("100", req3q(100, q_t([C("Setting", S("ANSI_NULLS"), "f"), assign(tv("@q"), tv("@@ROWCOUNT"))]),
                  q_p([assign(pv("p_q"), num(1))])), "unconfirmed"),
    # The T-SQL side lowers; the PL/pgSQL procedure returns a value.
    ("101", req3q(101, q_t([ABORT]), q_p([C("NStmt", C("Return", num(1)))])), "unconfirmed"),
    # A decimal(18,2) parameter with a default: the type is named, sanitised.
    ("102", req3(102, a=proc(["dbo", "pd"], [C("Param", S("@d"), DEC, C("In"), num(1))], [], []),
                 b=proc(["pd"], [C("Param", S("d"), DEC, C("In"), num(1))], [], []),
                 dep="0", cat="-", cert=f"{hx('%status')}:{hx('%status')}"), "unconfirmed"),
    # UPDATE acct a SET ... on the PL/pgSQL side.
    ("103", req3d(103, b=proc(["bump"], [param("p_id", "In"), param("p_n", "In")], [], [
        C("NStmt", C("Update", L(S("acct")), S("a"), L(C("Set", L(S("qty")), num(1))), L(), NONE))])),
     "unconfirmed"),
]
# C2c: typed values. T-SQL dbo.Ev (Id int PRIMARY KEY, D date, T datetime2(6),
# B bit NOT NULL, N bigint, S smallint DEFAULT 0) is the target table ev; the
# pair is eval/values' procedure as the converter writes it (ExamplesC2c.lean).
V_DEP = f"0;{hx('dbo.Ev')}:{hx('ev')}"
V_CAT = (f"{hx('ev')}={hx('id')}.ink.{hx('Id')}/{hx('d')}.d.{hx('D')}/{hx('t')}.t6.{hx('T')}"
         f"/{hx('b')}.bn.{hx('B')}/{hx('n')}.j.{hx('N')}/{hx('s')}.h.{hx('S')}.i0")
V_CERT = ",".join(f"{hx(a)}:{hx(b)}" for a, b in
                  [("@id", "p_id"), ("@big", "p_big"), ("@flag", "p_flag"), ("@when", "p_when"),
                   ("@ts", "p_ts"), ("%status", "%status"), ("@d", "v_d"), ("@b", "v_b"),
                   ("@s", "v_s"), ("@n", "v_n"), ("@t", "v_t"), ("@i", "v_i")])
DATE, DT2_6, DT2_7, TS6, BIT, BOOL, BIGINT, SMALLINT = (
    ty("DATE"), ty("DATETIME2", "6"), ty("DATETIME2", "7"), ty("TIMESTAMP", "6"), ty("BIT"),
    ty("BOOLEAN"), ty("BIGINT"), ty("SMALLINT"))
TRUE, FALSE = C("Special", S("TRUE")), C("Special", S("FALSE"))


def cast(e, t):
    return C("Cast", e, t)


def dateadd(part, n, d):
    return fn("DATEADD", pv(part), n, d)


def make_interval(part, n):
    z = num(0)
    return fn("make_interval", n if part == "years" else z, n if part == "months" else z, z,
              n if part == "days" else z, z, z, C("Num", S("0.0")))


def if_(c, then):
    return C("If", c, L(*then), L())


def decl(x, t, v=NONE):
    return C("NStmt", C("Declare", S(x), t, v))


V_KEY_T = bin_("=", col("Id"), tv("@id"))
V_KEY_P = bin_("=", pv("Id"), pv("p_id"))


def v_t(extra=(), t_decl=DT2_6, s_value=num(5), b_value=num(0)):
    return proc(["dbo", "p"], [param("@id", "In"), sparam("@big", "In", BIGINT), sparam("@flag", "In", BIT),
                               sparam("@when", "In", DATE), sparam("@ts", "InOut", DT2_6)], [], [
        ABORT, C("Setting", S("NOCOUNT"), "t"),
        decl("@d", DATE, string("2024-01-31")), decl("@b", BIT, num(1)), decl("@s", SMALLINT, s_value),
        decl("@n", BIGINT), decl("@t", t_decl, string("2024-01-01T23:59:00.123456")), decl("@i", INT),
        assign(tv("@n"), bin_("+", tv("@big"), num(1))),
        assign(tv("@b"), b_value),
        assign(tv("@d"), dateadd("day", num(1), tv("@d"))),
        assign(tv("@d"), dateadd("month", tv("@id"), tv("@d"))),
        assign(tv("@d"), dateadd("year", C("Num", S("-1")), tv("@d"))),
        assign(tv("@t"), dateadd("day", num(2), tv("@t"))),
        assign(tv("@i"), fn("DATEDIFF", pv("day"), tv("@d"), tv("@t"))),
        assign(tv("@d"), cast(tv("@t"), DATE)),
        if_(bin_("=", tv("@b"), num(1)), [assign(tv("@i"), num(1))]),
        if_(bin_("AND", bin_("=", tv("@flag"), num(1)), bin_("<", tv("@d"), tv("@when"))),
            [assign(tv("@i"), num(4))]),
        if_(bin_("=", tv("@d"), string("2024-01-01")), [assign(tv("@i"), num(5))]),
        if_(bin_(">=", tv("@t"), string("2024-01-01T00:00:00")), [assign(tv("@i"), num(6))]),
        if_(C("IsNull", tv("@ts"), "f"), [assign(tv("@ts"), tv("@t"))]),
        ins(["dbo", "Ev"], ["Id", "D", "T", "B", "N"], [[tv("@id"), tv("@d"), tv("@t"), num(1), tv("@n")]]),
        upd(["dbo", "Ev"], [("B", num(0)), ("D", dateadd("day", num(1), col("D")))], V_KEY_T),
        C("NStmt", C("SSelect", sel([item(col("N"), assign_=S("@n")), item(col("D"), assign_=S("@d"))],
                                    ["dbo", "Ev"], V_KEY_T))),
        *extra])


def v_p(extra=()):
    return proc(["p"], [param("p_id", "In"), sparam("p_big", "In", BIGINT), sparam("p_flag", "In", BOOL),
                        sparam("p_when", "In", DATE), sparam("p_ts", "InOut", TS6)],
                [C("Declare", S("v_d"), DATE, NONE), C("Declare", S("v_b"), BOOL, NONE),
                 C("Declare", S("v_s"), SMALLINT, NONE), C("Declare", S("v_n"), BIGINT, NONE),
                 C("Declare", S("v_t"), TS6, NONE), C("Declare", S("v_i"), INT, NONE)], [
        assign(pv("v_d"), string("2024-01-31")), assign(pv("v_b"), TRUE), assign(pv("v_s"), num(5)),
        assign(pv("v_t"), string("2024-01-01T23:59:00.123456")),
        assign(pv("v_n"), bin_("+", pv("p_big"), num(1))),
        assign(pv("v_b"), FALSE),
        assign(pv("v_d"), bin_("+", pv("v_d"), num(1))),
        assign(pv("v_d"), cast(bin_("+", pv("v_d"), make_interval("months", pv("p_id"))), DATE)),
        assign(pv("v_d"), cast(bin_("+", pv("v_d"), make_interval("years", C("Num", S("-1")))), DATE)),
        assign(pv("v_t"), bin_("+", pv("v_t"), make_interval("days", num(2)))),
        assign(pv("v_i"), bin_("-", cast(pv("v_t"), DATE), cast(pv("v_d"), DATE))),
        assign(pv("v_d"), cast(pv("v_t"), DATE)),
        if_(bin_("=", pv("v_b"), TRUE), [assign(pv("v_i"), num(1))]),
        if_(bin_("AND", bin_("=", pv("p_flag"), TRUE), bin_("<", pv("v_d"), pv("p_when"))),
            [assign(pv("v_i"), num(4))]),
        if_(bin_("=", pv("v_d"), string("2024-01-01")), [assign(pv("v_i"), num(5))]),
        if_(bin_(">=", pv("v_t"), string("2024-01-01T00:00:00")), [assign(pv("v_i"), num(6))]),
        if_(C("IsNull", pv("p_ts"), "f"), [assign(pv("p_ts"), pv("v_t"))]),
        ins(["Ev"], ["Id", "D", "T", "B", "N"], [[pv("p_id"), pv("v_d"), pv("v_t"), TRUE, pv("v_n")]]),
        C("Txn", S("COMMIT"), NONE),
        upd(["Ev"], [("B", FALSE), ("D", bin_("+", pv("D"), num(1)))], V_KEY_P),
        C("Txn", S("COMMIT"), NONE),
        for_block([item(pv("N"), alias=S("sqldiff_c1")), item(pv("D"), alias=S("sqldiff_c2"))], ["Ev"], V_KEY_P,
                  [field("v_n", "sqldiff_c1"), field("v_d", "sqldiff_c2")]),
        *extra])


def req3v(i, a=None, b=None, cat=V_CAT):
    return req3(i, a=v_t() if a is None else a, b=v_p() if b is None else b, dep=V_DEP, cat=cat,
                cert=V_CERT)


cases += [
    ("104", req3v(104), "confirmed"),
    # A datetime2(7) local: its literal is a string, the conversion's a timestamp.
    ("105", req3v(105, a=v_t(t_decl=DT2_7)), "unconfirmed"),
    # An int into the smallint local: a narrowing store.
    ("106", req3v(106, a=v_t(s_value=tv("@id"))), "unconfirmed"),
    # A bit stored from 2.
    ("107", req3v(107, a=v_t(b_value=num(2))), "unconfirmed"),
    # A literal that is not ISO 8601, and DATEADD(hour, ...).
    ("108", req3v(108, a=v_t(extra=[assign(tv("@d"), string("01/02/2024"))])), "unconfirmed"),
    ("109", req3v(109, a=v_t(extra=[assign(tv("@t"), dateadd("hour", num(1), tv("@t")))])), "unconfirmed"),
    # A constant beyond bigint; a bigint column written from an int is fine, a
    # smallint column from an int is not.
    ("110", req3v(110, a=v_t(extra=[ins(["dbo", "Ev"], ["Id", "B", "S"], [[num(2), num(1), tv("@id")]])])),
     "unconfirmed"),
    # A malformed SPEC: the catalog does not decode; a bit key column: not usable.
    ("111", req3v(111, cat=V_CAT.replace(".t6.", ".t.")), "unconfirmed"),
    ("112", req3v(112, cat=V_CAT.replace(".bn.", ".bnk.")), "unconfirmed"),
]
# Review of C2c: each pair below was confirmed before the fix.
# B1: PostgreSQL `date + make_interval(...)` is a timestamp; `+ 1` on it is
# no date arithmetic (PostgreSQL: no operator timestamp + integer).
def f1_t():
    return proc(["dbo", "f1"], [param("@id", "In"), sparam("@d", "In", DATE), sparam("@r", "InOut", DATE)], [],
                [ABORT, assign(tv("@r"), dateadd("day", num(1), dateadd("month", tv("@id"), tv("@d"))))])


def f1_p(cast_date):
    inner = bin_("+", pv("p_d"), make_interval("months", pv("p_id")))
    return proc(["f1"], [param("p_id", "In"), sparam("p_d", "In", DATE), sparam("p_r", "InOut", DATE)], [],
                [assign(pv("p_r"), bin_("+", cast(inner, DATE) if cast_date else inner, num(1)))])


F1_CERT = ",".join(f"{hx(a)}:{hx(b)}" for a, b in [("@id", "p_id"), ("@d", "p_d"), ("@r", "p_r"), ("%status", "%status")])
# B2: an int argument into a smallint callee (PostgreSQL 42883).
CS_DEP = f"0,{hx('dbo.cs')}:{hx('cs')}"


def cs_cat(t):
    return f"{hx('cs')}:i:{hx(t)}"


CS_T = proc(["dbo", "f5"], [param("@i", "In")], [], [ABORT, call(["dbo", "cs"], (tv("@i"), "f"))])
CS_P = proc(["f5"], [param("p_i", "In")], [], [call(["cs"], (pv("p_i"), "f"))])
CS_CERT = ",".join(f"{hx(a)}:{hx(b)}" for a, b in [("@i", "p_i"), ("%status", "%status")])
# B3: `@b <> 1` against `b <> TRUE` names ASM-TSQL-ANSI-NULLS.
AN_T = proc(["dbo", "an"], [sparam("@b", "In", BIT), param("@r", "InOut")], [],
            [ABORT, assign(tv("@r"), num(0)), if_(bin_("<>", tv("@b"), num(1)), [assign(tv("@r"), num(1))])])
AN_P = proc(["an"], [sparam("p_b", "In", BOOL), param("p_r", "InOut")], [],
            [assign(pv("p_r"), num(0)), if_(bin_("<>", pv("p_b"), TRUE), [assign(pv("p_r"), num(1))])])
AN_CERT = ",".join(f"{hx(a)}:{hx(b)}" for a, b in [("@b", "p_b"), ("@r", "p_r"), ("%status", "%status")])
# M1: a datetime2(6) value through a T-SQL datetime local and a PL/pgSQL
# timestamp local (SQL Server .123333, PostgreSQL .123456).
M1_T = proc(["dbo", "f3"], [sparam("@t", "In", DT2_6), sparam("@r", "InOut", DT2_6)], [],
            [ABORT, decl("@x", ty("DATETIME")), assign(tv("@x"), tv("@t")), assign(tv("@r"), tv("@x"))])
M1_P = proc(["f3"], [sparam("p_t", "In", ty("TIMESTAMP")), sparam("p_r", "InOut", ty("TIMESTAMP"))],
            [C("Declare", S("v_x"), ty("TIMESTAMP"), NONE)],
            [assign(pv("v_x"), pv("p_t")), assign(pv("p_r"), pv("v_x"))])
M1_CERT = ",".join(f"{hx(a)}:{hx(b)}" for a, b in [("@t", "p_t"), ("@r", "p_r"), ("@x", "v_x"), ("%status", "%status")])
cases += [
    ("113", req3(113, a=f1_t(), b=f1_p(False), dep="0", cat="-", cert=F1_CERT), "unconfirmed"),
    ("114", req3(114, a=f1_t(), b=f1_p(True), dep="0", cat="-", cert=F1_CERT), "confirmed"),
    ("115", req3(115, a=CS_T, b=CS_P, dep=CS_DEP, cat=cs_cat("SMALLINT"), cert=CS_CERT), "unconfirmed"),
    ("116", req3(116, a=CS_T, b=CS_P, dep=CS_DEP, cat=cs_cat("INTEGER"), cert=CS_CERT), "confirmed"),
    ("117", req3(117, a=CS_T, b=CS_P, dep=CS_DEP, cat=f"{hx('cs')}:i", cert=CS_CERT), "unconfirmed"),
    ("118", req3(118, a=AN_T, b=AN_P, dep="0", cat="-", cert=AN_CERT), "confirmed"),
    ("119", req3(119, a=M1_T, b=M1_P, dep="0", cat="-", cert=M1_CERT), "unconfirmed"),
]
# Re-review of C2c. R1: a type outside the slice corresponds to nothing.
# T-SQL `time` is time(7) and keeps .1234567 < .1234568 (1); PostgreSQL's
# time(6) rounds both to .123457 (0).
TIME = ty("TIME")


def tm_t(stores):
    sets = [assign(tv("@a"), string("12:00:00.1234567")), assign(tv("@b"), string("12:00:00.1234568"))]
    return proc(["dbo", "tm"], [param("@r", "InOut")], [],
                [ABORT, decl("@a", TIME), decl("@b", TIME)] + (sets if stores else [])
                + [assign(tv("@r"), num(0)), if_(bin_("<", tv("@a"), tv("@b")), [assign(tv("@r"), num(1))])])


def tm_p(stores):
    sets = [assign(pv("v_a"), string("12:00:00.1234567")), assign(pv("v_b"), string("12:00:00.1234568"))]
    return proc(["tm"], [param("p_r", "InOut")],
                [C("Declare", S("v_a"), TIME, NONE), C("Declare", S("v_b"), TIME, NONE)],
                (sets if stores else [])
                + [assign(pv("p_r"), num(0)), if_(bin_("<", pv("v_a"), pv("v_b")), [assign(pv("p_r"), num(1))])])


TM_CERT = ",".join(f"{hx(a)}:{hx(b)}" for a, b in [("@r", "p_r"), ("@a", "v_a"), ("@b", "v_b"), ("%status", "%status")])
# R2: a string argument must fit the callee's declared length: `EXEC dbo.q
# @v` with @v varchar(10) holding 'abcdef' reaches q(@s varchar(3)) as
# 'abc' in SQL Server and whole in PostgreSQL.
Q_DEP = f"0,{hx('dbo.q')}:{hx('q')}"


def q_cat(t, *args):
    return f"{hx('q')}:i:" + ".".join([hx(t)] + [hx(a) for a in args])


def q_t(arg, decls=()):
    return proc(["dbo", "f6"], [param("@i", "In")], [], [ABORT] + list(decls) + [call(["dbo", "q"], (arg, "f"))])


def q_p(arg, decls=()):
    return proc(["f6"], [param("p_i", "In")], list(decls), [call(["q"], (arg, "f"))])


Q_CERT = ",".join(f"{hx(a)}:{hx(b)}" for a, b in [("@i", "p_i"), ("@v", "v_v"), ("%status", "%status")])
Q10_DECL_T = [decl("@v", VC10)]
Q10_DECL_P = [C("Declare", S("v_v"), VC10, NONE)]
Q3_DECL_T = [decl("@v", VC3)]
Q3_DECL_P = [C("Declare", S("v_v"), VC3, NONE)]
cases += [
    ("120", req3(120, a=tm_t(True), b=tm_p(True), dep="0", cat="-", cert=TM_CERT), "unconfirmed"),
    ("121", req3(121, a=tm_t(False), b=tm_p(False), dep="0", cat="-", cert=TM_CERT), "unconfirmed"),
    ("122", req3(122, a=q_t(tv("@v"), Q10_DECL_T), b=q_p(pv("v_v"), Q10_DECL_P), dep=Q_DEP,
                 cat=q_cat("VARCHAR", "3"), cert=Q_CERT), "unconfirmed"),
    ("123", req3(123, a=q_t(tv("@v"), Q3_DECL_T), b=q_p(pv("v_v"), Q3_DECL_P), dep=Q_DEP,
                 cat=q_cat("VARCHAR", "3"), cert=Q_CERT), "confirmed"),
    ("124", req3(124, a=q_t(string("abcd")), b=q_p(string("abcd")), dep=Q_DEP,
                 cat=q_cat("VARCHAR", "3"), cert=Q_CERT), "unconfirmed"),
    ("125", req3(125, a=q_t(string("abc")), b=q_p(string("abc")), dep=Q_DEP,
                 cat=q_cat("TEXT"), cert=Q_CERT), "unconfirmed"),
]
# C3d: query blocks. T-SQL dbo.Ord (Id int PRIMARY KEY, Qty int NULL, CustId
# int NULL) and dbo.Cust (Id int PRIMARY KEY, Score int NULL) are the target
# tables ord and cust; joins, aliases and qualified names in the C3b shapes.
J_DEP = f"0;{hx('dbo.Ord')}:{hx('ord')},{hx('dbo.Cust')}:{hx('cust')}"
J_CAT = (f"{hx('ord')}={hx('id')}.ink.{hx('Id')}/{hx('qty')}.i.{hx('Qty')}/{hx('custid')}.i.{hx('CustId')},"
         f"{hx('cust')}={hx('id')}.ink.{hx('Id')}/{hx('score')}.i.{hx('Score')}")


def tbl(name, alias=None):
    return C("Table", L(*map(S, name)), NONE if alias is None else S(alias), L())


def join(kind, left, right, on=NONE):
    return C("Join", S(kind), left, right, on)


def sel_f(items, froms, where=NONE, into=()):
    return C("Select", L(), "f", L(), "f", NONE, L(*items), L(*into), "f",
             L(*froms), where, L(), NONE, L(), NONE, NONE, L(), NONE)


def qn(a, c):
    return C("Name", L(S(a), S(c)))


def oc_t(on):
    return [join("INNER", tbl(["dbo", "Ord"], "o"), tbl(["dbo", "Cust"], "c"), on)]


def oc_p(on):
    return [join("INNER", tbl(["ord"], "o"), tbl(["cust"], "c"), on)]


def j_t(body, name=("dbo", "look")):
    return proc(list(name), [param("@id", "In"), param("@q", "InOut")], [], body)


def j_p(body, name=("look",)):
    return proc(list(name), [param("p_id", "In"), param("p_q", "InOut")], [], body)


def req3j(i, a, b):
    return req3(i, a=a, b=b, dep=J_DEP, cat=J_CAT, cert=Q_CERT_J)


Q_CERT_J = ",".join(f"{hx(a)}:{hx(b)}" for a, b in [("@id", "p_id"), ("@q", "p_q"), ("%status", "%status")])
ON_KEY_T = bin_("=", qn("c", "Id"), qn("o", "CustId"))
ON_KEY_P = bin_("=", qn("c", "id"), qn("o", "custid"))
J_SEL_T = C("NStmt", C("SSelect", sel_f([item(qn("c", "Score"), assign_=S("@q"))], oc_t(ON_KEY_T),
                                        bin_("=", qn("o", "Id"), tv("@id")))))
J_FOR_P = C("NBlock", C("Block", L(C("Declare", S("sqldiff_r"), C("Ty", S("RECORD"), L()), NONE)),
                        L(C("ForQuery", S("sqldiff_r"),
                            sel_f([item(qn("c", "score"), alias=S("sqldiff_c1"))], oc_p(ON_KEY_P),
                                  bin_("=", qn("o", "id"), pv("p_id"))),
                            L(field("p_q", "sqldiff_c1"))))))


def j_scalar_t(where):
    return assign(tv("@q"), C("Subquery", sel_f([item(qn("c", "Score"))], oc_t(ON_KEY_T), where)))


def j_scalar_p(where):
    return assign(pv("p_q"), C("Subquery", sel_f([item(qn("c", "score"))], oc_p(ON_KEY_P), where)))


cases += [
    # SELECT @q = c.Score over a join by key against the FOR block.
    ("126", req3j(126, j_t([J_SEL_T]), j_p([J_FOR_P])), "confirmed"),
    # Joined on a column that is no key: the block may keep several rows.
    ("127", req3j(127, j_t([C("NStmt", C("SSelect", sel_f([item(qn("c", "Score"), assign_=S("@q"))],
                                                          oc_t(bin_("=", qn("c", "Score"), qn("o", "Qty"))),
                                                          bin_("=", qn("o", "Id"), tv("@id")))))]),
                  j_p([J_FOR_P])), "unconfirmed"),
    # A scalar subquery over a join that may keep two rows: in, as the
    # cardinality error; without XACT_ABORT ON only on the C1 theorem.
    ("128", req3j(128, j_t([j_scalar_t(bin_(">", qn("o", "Qty"), tv("@id")))]),
                  j_p([j_scalar_p(bin_(">", qn("o", "qty"), pv("p_id")))])), "confirmed"),
    ("129", req3j(129, j_t([ABORT, j_scalar_t(bin_(">", qn("o", "Qty"), tv("@id")))]),
                  j_p([j_scalar_p(bin_(">", qn("o", "qty"), pv("p_id")))])), "confirmed"),
    # An unqualified name that is a column of both tables (209, 42702).
    ("130", req3j(130, j_t([C("NStmt", C("SSelect", sel_f([item(qn("c", "Score"), assign_=S("@q"))],
                                                          oc_t(ON_KEY_T), bin_("=", col("Id"), tv("@id")))))]),
                  j_p([J_FOR_P])), "unconfirmed"),
    # The PL/pgSQL qualifier is the routine's name (eval/queries Q12).
    ("131", req3j(131, j_t([J_SEL_T]), j_p([J_FOR_P], name=("o",))), "unconfirmed"),
    # A LEFT JOIN.
    ("132", req3j(132, j_t([C("NStmt", C("SSelect", sel_f([item(qn("c", "Score"), assign_=S("@q"))],
                                                          [join("LEFT", tbl(["dbo", "Ord"], "o"),
                                                                tbl(["dbo", "Cust"], "c"), ON_KEY_T)],
                                                          bin_("=", qn("o", "Id"), tv("@id")))))]),
                  j_p([J_FOR_P])), "unconfirmed"),
]
THEOREMS = {
    "114": "Sqldiff.Mirror.check_sound_c2a",
    "116": "Sqldiff.Mirror.check_sound_c2a",
    "123": "Sqldiff.Mirror.check_sound",
    "118": "Sqldiff.Mirror.check_sound_c2a",
    "126": "Sqldiff.Mirror.check_sound_c2a",
    "128": "Sqldiff.Mirror.check_sound",
    "129": "Sqldiff.Mirror.check_sound_c2a",
    "104": "Sqldiff.Mirror.check_sound_c2a",
    "91": "Sqldiff.Mirror.check_sound_c2a",
    "83": "Sqldiff.Mirror.check_sound_c2a",
    "86": "Sqldiff.Mirror.check_sound_c2a",
    "87": "Sqldiff.Mirror.check_sound_c2a",
    "89": "Sqldiff.Mirror.check_sound_c2a",
    "82": "Sqldiff.Mirror.check_sound",
    "69": "Sqldiff.Mirror.check_sound_c2a",
    "70": "Sqldiff.Mirror.check_sound",
    "71": "Sqldiff.Mirror.check_sound_c2a",
    "74": "Sqldiff.Mirror.check_sound_c2a",
    "60": "Sqldiff.Mirror.check_sound_c2a",
    "61": "Sqldiff.Mirror.check_sound_c2a",
    "62": "Sqldiff.Mirror.check_sound_c2a",
    "64": "Sqldiff.Mirror.check_sound_c2a",
    "31": "Sqldiff.Mirror.check_sound",
    "40": "Sqldiff.Mirror.check_sound",
    "41": "Sqldiff.Mirror.check_sound_c2a",
    "42": "Sqldiff.Mirror.check_sound_c2a",
    "44": "Sqldiff.Mirror.check_sound_c2a",
    "50": "Sqldiff.Mirror.check_sound",
    "51": "Sqldiff.Mirror.check_sound_c2a",
    "52": "Sqldiff.Mirror.check_sound_c2a",
    "56": "Sqldiff.Mirror.check_sound_c2a",
    "59": "Sqldiff.Mirror.check_sound_c2a",
}
# The fields a confirmed /3 answer adds (Main.lean); none on /2 answers. An
# unconfirmed /3 answer whose routine is outside the spec adds `why` (0.13.0),
# the reason codes of Sqldiff/Mirror/Diagnose.lean; no other answer does.
EXTRA = {
    # (C2c) Without a catalog the callee's parameter types are not known either.
    "35": {"why": ["node:call:type"]},
    "36": {"why": ["node:call:name"]},
    # A T-SQL routine as the PL/pgSQL side: every construct named, in order.
    "38": {"why": ["stmt:declare:pg", "expr:pg-var", "stmt:assign:target", "node:call:name",
                   "stmt:return:pg-proc"]},
    "45": {"why": ["stmt:update:table:unknown", "stmt:insert:table:unknown"]},
    "46": {"why": ["stmt:update:table:unknown", "stmt:insert:table:unknown"]},
    "48": {"why": ["expr:name:unresolved"]},
    "49": {"why": ["stmt:dml:rowcounts"]},
    "58": {"why": ["store:assign:varchar_20", "expr:fn:isnull"]},
    "65": {"why": ["expr:fn:error:error_number"]},
    "66": {"why": ["node:raise:level"]},
    "68": {"why": ["routine:param:default"]},
    "72": {"why": ["node:raise:opts"]},
    "73": {"why": ["node:txn:begin", "node:txn:commit", "node:txn:rollback"]},
    "75": {"why": ["node:try:idiom", "node:txn:begin", "node:txn:commit"]},
    "76": {"why": ["store:assign:varchar_20"]},
    "77": {"why": ["store:assign:varchar_20", "expr:fn:upper"]},
    "78": {"why": ["store:declare:varchar_3"]},
    "79": {"why": ["store:return:varchar_5"]},
    "80": {"why": ["store:call", "node:call:type"]},
    "81": {"why": ["store:declare:varchar_3"]},
    "84": {"why": ["stmt:select:many-rows"]},
    "90": {"why": ["node:for-query"]},
    "92": {"why": ["stmt:insert:value", "stmt:update:value", "expr:fn:rtrim"]},
    "93": {"why": ["stmt:insert:table:not-plain", "stmt:update:table:not-plain", "stmt:select:table:not-plain"]},
    "94": {"why": ["stmt:insert:table:not-plain", "stmt:update:table:not-plain", "stmt:select:table:not-plain"]},
    "95": {"why": ["stmt:insert:value"]},
    "96": {"why": ["stmt:insert:value"]},
    "99": {"why": ["stmt:select:result-set"]},
    "100": {"why": ["node:setting:ansi_nulls", "expr:var:global:rowcount"]},
    "101": {"why": ["stmt:return:pg-proc"]},
    "102": {"why": ["routine:param:decimal_18_2"]},
    "103": {"why": ["stmt:update:alias"]},
    "91": {"scalars": "c2a",
           "residual": ["ASM-TSQL-ANSI-NULLS", "ASM-STATE-AFTER-ERROR", "ASM-TSQL-STRING-ARGS"],
           "statements": "c3b"},
    "104": {"scalars": "c2a",
            "residual": ["ASM-TSQL-ARITH-ERRORS", "ASM-TSQL-ANSI-NULLS", "ASM-TSQL-STRING-ARGS",
                         "ASM-DATEADD-RANGE", "ASM-DATEADD-YEAR-END", "ASM-NO-CALLER-TXN"],
            "statements": "c3b", "errors": "c4"},
    "105": {"why": ["store:declare:datetime2_7", "type:datetime2_7", "stmt:insert:value"]},
    "114": {"scalars": "c2a",
            "residual": ["ASM-TSQL-STRING-ARGS", "ASM-DATEADD-RANGE"],
            "statements": "c1"},
    "115": {"why": ["node:call:type"]},
    "116": {"scalars": "c2a",
            "residual": ["ASM-STATE-AFTER-ERROR", "ASM-TSQL-STRING-ARGS"],
            "statements": "c1"},
    "117": {"why": ["node:call:type"]},
    "118": {"scalars": "c2a", "residual": ["ASM-TSQL-ANSI-NULLS", "ASM-TSQL-STRING-ARGS"],
            "statements": "c1"},
    "119": {"why": ["store:assign:datetime"]},
    # Re-review of C2c: a string into `time`; an argument longer than, or
    # with no bound against, the callee's declared length.
    "120": {"why": ["store:assign:time"]},
    "122": {"why": ["node:call:type"]},
    "123": {"scalars": "uninterpreted", "statements": "c1"},
    "124": {"why": ["node:call:type"]},
    "125": {"why": ["node:call:type"]},
    # C3d: query blocks.
    "126": {"scalars": "c2a", "residual": ["ASM-TSQL-ANSI-NULLS", "ASM-TSQL-STRING-ARGS"],
            "statements": "c3b"},
    "127": {"why": ["stmt:select:many-rows"]},
    "128": {"scalars": "uninterpreted", "statements": "c3b"},
    "129": {"scalars": "c2a", "residual": ["ASM-TSQL-ANSI-NULLS", "ASM-TSQL-STRING-ARGS"],
            "statements": "c3b"},
    "130": {"why": ["name:ambiguous"]},
    "131": {"why": ["stmt:select:from:qualifier"]},
    "132": {"why": ["stmt:select:from:join:left"]},
    "106": {"why": ["store:declare:smallint"]},
    "107": {"why": ["store:assign:bit", "expr:bit-literal"]},
    "108": {"why": ["store:assign:date", "expr:date-literal"]},
    "109": {"why": ["expr:dateadd:part"]},
    "110": {"why": ["stmt:insert:value"]},
    "112": {"why": ["stmt:insert:table:not-plain", "stmt:update:table:not-plain",
                    "stmt:select:table:not-plain"]},
    "83": {"scalars": "c2a",
           "residual": ["ASM-TSQL-ANSI-NULLS", "ASM-TSQL-STRING-ARGS"],
           "statements": "c3b"},
    "86": {"scalars": "c2a", "residual": ["ASM-TSQL-STRING-ARGS"], "statements": "c3b"},
    "87": {"scalars": "c2a",
           "residual": ["ASM-TSQL-ANSI-NULLS", "ASM-TSQL-STRING-ARGS"],
           "statements": "c3b"},
    "89": {"scalars": "c2a",
           "residual": ["ASM-TSQL-ARITH-ERRORS", "ASM-TSQL-STRING-ARGS"],
           "statements": "c3b"},
    "31": {"scalars": "uninterpreted", "statements": "c1"},
    "40": {"scalars": "uninterpreted", "statements": "c1"},
    # The call to log_value can fail after the callee wrote (0.8.0).
    "41": {"scalars": "c2a",
           "residual": ["ASM-TSQL-ARITH-ERRORS", "ASM-STATE-AFTER-ERROR", "ASM-TSQL-STRING-ARGS"],
           "statements": "c1"},
    "69": {"scalars": "c2a", "residual": ["ASM-TSQL-STRING-ARGS"], "statements": "c1"},
    "70": {"scalars": "uninterpreted", "statements": "c1"},
    "82": {"scalars": "uninterpreted", "statements": "c1"},
    "71": {"scalars": "c2a",
           "residual": ["ASM-STATE-AFTER-ERROR", "ASM-TSQL-STRING-ARGS"],
           "statements": "c1"},
    "74": {"scalars": "c2a",
           "residual": ["ASM-TSQL-ARITH-ERRORS", "ASM-STATE-AFTER-ERROR", "ASM-TSQL-STRING-ARGS",
                        "ASM-NO-CALLER-TXN"],
           "statements": "c3a", "errors": "c4"},
    "42": {"scalars": "c2a", "residual": ["ASM-TSQL-STRING-ARGS"], "statements": "c1"},
    "44": {"scalars": "c2a",
           "residual": ["ASM-TSQL-ARITH-ERRORS", "ASM-TSQL-ANSI-NULLS", "ASM-STATE-AFTER-ERROR",
                        "ASM-TSQL-STRING-ARGS"],
           "statements": "c3a"},
    "50": {"scalars": "uninterpreted", "statements": "c3a"},
    "51": {"scalars": "c2a", "residual": ["ASM-CONCAT-NULL", "ASM-TSQL-STRING-ARGS"], "statements": "c1"},
    "52": {"scalars": "c2a",
           "residual": ["ASM-TSQL-ANSI-NULLS", "ASM-BINARY-COLLATION", "ASM-TSQL-STRING-ARGS"],
           "statements": "c1"},
    "56": {"scalars": "c2a", "residual": ["ASM-TSQL-LEN-BMP", "ASM-TSQL-STRING-ARGS"], "statements": "c1"},
    "59": {"scalars": "c2a", "residual": ["ASM-TSQL-STRING-ARGS"], "statements": "c1"},
    "60": {"scalars": "c2a",
           "residual": ["ASM-TSQL-ARITH-ERRORS", "ASM-TSQL-STRING-ARGS", "ASM-CATCH-CORE-ERRORS",
                        "ASM-NO-CALLER-TXN"],
           "statements": "c1", "errors": "c4"},
    "61": {"scalars": "c2a",
           "residual": ["ASM-TSQL-STRING-ARGS", "ASM-CATCH-CORE-ERRORS", "ASM-NO-CALLER-TXN",
                        "ASM-THROW-ERRCODE"],
           "statements": "c3a", "errors": "c4"},
    "62": {"scalars": "c2a",
           "residual": ["ASM-TSQL-ARITH-ERRORS", "ASM-STATE-AFTER-ERROR", "ASM-TSQL-STRING-ARGS",
                        "ASM-CATCH-CORE-ERRORS", "ASM-NO-CALLER-TXN"],
           "statements": "c3a", "errors": "c4"},
    "64": {"scalars": "c2a",
           "residual": ["ASM-TSQL-ARITH-ERRORS", "ASM-STATE-AFTER-ERROR", "ASM-TSQL-STRING-ARGS",
                        "ASM-NO-CALLER-TXN"],
           "statements": "c3a", "errors": "c4"},
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
    if "why" in got:
        # The codes are what the README's table promises: ASCII `family:detail`,
        # distinct, at most 24, only on the two outside-spec reasons.
        assert got["proof"] == "unconfirmed" and got["reason"].endswith("is outside the C1 spec"), got
        assert got["why"] and len(got["why"]) <= 24 and len(set(got["why"])) == len(got["why"]), got
        assert all(re.fullmatch(r"[a-z0-9_/.-]+(:[a-z0-9_/.-]+)+", c) for c in got["why"]), got
    elif got["proof"] == "unconfirmed":
        assert not got["reason"].endswith("is outside the C1 spec"), got
print(f"gate protocol ok ({len(cases)} cases)")
