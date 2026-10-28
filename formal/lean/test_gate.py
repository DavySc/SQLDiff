"""Protocol tests for the built checker. sqldiff-check/2: confirmed only
for identical sides that decode under the checker's syntax schema.
sqldiff-check/3: the C1 mirror check, confirmed for a T-SQL procedure and
its PL/pgSQL mirror, with the C2a fields (`scalars`, `residual`) and the
C3a field (`statements`) on a confirmed answer; C3a requests with table
mappings in DEPLOYMENT and table entries in CATALOG; C2b string pairs,
with the collation fact in CATALOG and the string residuals; C4 pairs
(TRY/CATCH against EXCEPTION blocks, THROW against RAISE, the
rollback-and-rethrow idiom with COMMIT), with the C4 residuals and the
`errors` field. Anything else is unconfirmed, including malformed
requests.

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
THEOREMS = {
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
# The fields a confirmed /3 answer adds (Main.lean); none on /2 or unconfirmed answers.
EXTRA = {
    "31": {"scalars": "uninterpreted", "statements": "c1"},
    "40": {"scalars": "uninterpreted", "statements": "c1"},
    # The call to log_value can fail after the callee wrote (0.8.0).
    "41": {"scalars": "c2a", "residual": ["ASM-TSQL-ARITH-ERRORS", "ASM-STATE-AFTER-ERROR"],
           "statements": "c1"},
    "69": {"scalars": "c2a", "residual": ["ASM-TSQL-STRING-ARGS"], "statements": "c1"},
    "70": {"scalars": "uninterpreted", "statements": "c1"},
    "71": {"scalars": "c2a", "residual": ["ASM-STATE-AFTER-ERROR"], "statements": "c1"},
    "74": {"scalars": "c2a",
           "residual": ["ASM-TSQL-ARITH-ERRORS", "ASM-STATE-AFTER-ERROR", "ASM-NO-CALLER-TXN"],
           "statements": "c3a", "errors": "c4"},
    "42": {"scalars": "c2a", "residual": [], "statements": "c1"},
    "44": {"scalars": "c2a",
           "residual": ["ASM-TSQL-ARITH-ERRORS", "ASM-TSQL-ANSI-NULLS", "ASM-STATE-AFTER-ERROR"],
           "statements": "c3a"},
    "50": {"scalars": "uninterpreted", "statements": "c3a"},
    "51": {"scalars": "c2a", "residual": ["ASM-CONCAT-NULL", "ASM-TSQL-STRING-ARGS"], "statements": "c1"},
    "52": {"scalars": "c2a",
           "residual": ["ASM-TSQL-ANSI-NULLS", "ASM-BINARY-COLLATION", "ASM-TSQL-STRING-ARGS"],
           "statements": "c1"},
    "56": {"scalars": "c2a", "residual": ["ASM-TSQL-LEN-BMP", "ASM-TSQL-STRING-ARGS"], "statements": "c1"},
    "59": {"scalars": "c2a", "residual": ["ASM-TSQL-STRING-ARGS"], "statements": "c1"},
    "60": {"scalars": "c2a",
           "residual": ["ASM-TSQL-ARITH-ERRORS", "ASM-CATCH-CORE-ERRORS", "ASM-NO-CALLER-TXN"],
           "statements": "c1", "errors": "c4"},
    "61": {"scalars": "c2a",
           "residual": ["ASM-CATCH-CORE-ERRORS", "ASM-NO-CALLER-TXN", "ASM-THROW-ERRCODE"],
           "statements": "c3a", "errors": "c4"},
    "62": {"scalars": "c2a",
           "residual": ["ASM-TSQL-ARITH-ERRORS", "ASM-STATE-AFTER-ERROR", "ASM-CATCH-CORE-ERRORS",
                        "ASM-NO-CALLER-TXN"],
           "statements": "c3a", "errors": "c4"},
    "64": {"scalars": "c2a",
           "residual": ["ASM-TSQL-ARITH-ERRORS", "ASM-STATE-AFTER-ERROR", "ASM-NO-CALLER-TXN"],
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
print(f"gate protocol ok ({len(cases)} cases)")
