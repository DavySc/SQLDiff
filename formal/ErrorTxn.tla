------------------------------ MODULE ErrorTxn ------------------------------
(***************************************************************************)
(* Error and transaction semantics of T-SQL (SQL Server 2022), PL/pgSQL    *)
(* (PostgreSQL 18) and SQL PL (Db2 for i), design doc section 7 and 12.    *)
(*                                                                         *)
(* A routine is a small program over abstract statements (writes, trans-   *)
(* action control, raising, status checks and structured handlers).  Each  *)
(* write is identified by a tag; a fault plan says which writes fail and   *)
(* with which error kind.  The model interprets the program under one      *)
(* dialect and its settings and ends in an outcome:                        *)
(*                                                                         *)
(*   durable  tags committed (cannot be undone any more)                   *)
(*   open     tags pending in the transaction the caller is left with      *)
(*   raised   an error reached the caller                                  *)
(*   aborted  execution stopped before the end of the routine              *)
(*   code     return code / status set by the routine                     *)
(*   tc       T-SQL @@TRANCOUNT at exit (0 for the other dialects)         *)
(*   doomed   the caller's transaction can only be rolled back             *)
(*                                                                         *)
(* The cases (programs, settings, fault plans and expected outcomes) are   *)
(* generated from profiles/error-txn.toml into ProfileCases.tla.  With     *)
(* Explore = FALSE, TLC checks that every case ends in its expected        *)
(* outcome.  With Explore = TRUE, TLC runs every case's program under      *)
(* every fault plan and checks the safety invariants and properties below. *)
(*                                                                         *)
(* Error kinds in fault plans:                                             *)
(*   "stmt"        ordinary runtime error (constraint violation, ...)      *)
(*   "batch"       T-SQL error that aborts the batch with XACT_ABORT OFF   *)
(*                 (V10: no published list; the pack keeps one)            *)
(*   "uncatchable" T-SQL severity >= 20 or attention; PostgreSQL           *)
(*                 QUERY_CANCELED / ASSERT_FAILURE, which WHEN OTHERS      *)
(*                 does not catch (V18)                                    *)
(*   "notfound"    no row qualified; a warning, never an error             *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets, TLC, ProfileCases

CONSTANT Explore

VARIABLES c, plan, st

vars == <<c, plan, st>>

Cs == Cases[c]
D  == Cs.dialect

\* FaultKinds(d), the fault kinds each dialect has, comes from ProfileCases.

\* The caller's own pending work, if the routine runs inside a caller transaction.
CallerTag == "caller"
InitOpen  == IF Cs.caller_txn THEN {CallerTag} ELSE {}
InitTc    == IF D = "tsql" /\ Cs.caller_txn THEN 1 ELSE 0

\* Db2 for i without commitment control: no transaction, nothing is ever undone.
NoCommit == D = "db2i" /\ Cs.commit = "NONE"

Frame(body, kind, blk, snap, owner) ==
    [body |-> body, pc |-> 1, kind |-> kind, handler |-> blk.handler,
     atomic |-> blk.atomic, onerr |-> blk.onerr, snap |-> snap, owner |-> owner]

RoutineBlk == [handler |-> "none", atomic |-> FALSE, onerr |-> <<>>]

Fault(id) == IF id \in DOMAIN plan THEN plan[id] ELSE "ok"

Tags(w) == IF w.rows = 1 THEN {w.id} ELSE {w.id \o ".1", w.id \o ".2"}

-----------------------------------------------------------------------------
(* State helpers.  A state is a record so that the semantics compose as    *)
(* plain operators.                                                        *)

Ok(s) == [s EXCEPT !.lastErr = FALSE, !.lastNF = FALSE]

\* Where a successful write lands.
Put(s, tags) ==
    IF NoCommit \/ (D = "tsql" /\ s.tc = 0)
    THEN [s EXCEPT !.durable = @ \cup tags]
    ELSE [s EXCEPT !.open = @ \cup tags]

Abort(s) == [s EXCEPT !.raised = TRUE, !.aborted = TRUE, !.done = TRUE]

NoSp == [x \in {} |-> {}]

\* Restore the pending set to snapshot x.  Savepoints set after x are gone.
Restore(s, x) ==
    [s EXCEPT !.open = x,
              !.sp = [n \in {m \in DOMAIN s.sp : s.sp[m] \subseteq x} |-> s.sp[n]]]

\* End of the transaction: every savepoint is released.
EndTxn(s) == [s EXCEPT !.sp = NoSp]

TsqlRollbackAll(s) == EndTxn([s EXCEPT !.open = {}, !.tc = 0, !.doomed = FALSE])

\* Replace frame t (a TRY, exception block or handler compound) by a frame
\* that runs its handler body; execution then continues after the block.
\* The handler belongs to the block: it keeps the block's ATOMIC flag and its
\* entry snapshot, so an unhandled error in the handler of an ATOMIC
\* compound rolls back the whole compound.
Catch(s, t) ==
    [s EXCEPT !.stack = SubSeq(@, 1, t - 1)
                        \o <<Frame(@[t].onerr, "catch", @[t], @[t].snap, t)>>]

Innermost(s, P(_)) ==
    LET idx == {i \in 1..Len(s.stack) : P(i)}
    IN  IF idx = {} THEN 0 ELSE CHOOSE i \in idx : \A j \in idx : j <= i

Outermost(s, P(_)) ==
    LET idx == {i \in 1..Len(s.stack) : P(i)}
    IN  IF idx = {} THEN 0 ELSE CHOOSE i \in idx : \A j \in idx : i <= j

\* A Db2 handler does not handle conditions raised in its own body: frames
\* from its compound up to the handler body are not eligible.
Excluded(s, j) ==
    \E k \in 1..Len(s.stack) : s.stack[k].owner > 0 /\ s.stack[k].owner <= j /\ j < k

-----------------------------------------------------------------------------
(* Failure of the current statement, per dialect.                          *)

\* T-SQL.  XACT_ABORT ON or a batch-aborting error rolls back and ends the
\* batch; with TRY the error goes to CATCH and leaves the transaction
\* uncommittable.  THROW ends the batch even with XACT_ABORT OFF but leaves
\* the transaction open.  RAISERROR never honours XACT_ABORT.
TsqlFail(s, k) ==
    LET t        == Innermost(s, LAMBDA i : s.stack[i].kind = "try")
        abortish == k = "batch" \/ (Cs.xact_abort /\ k \in {"stmt", "throw"})
    IN  IF k = "uncatchable" THEN Abort(TsqlRollbackAll(s))
        ELSE IF t > 0
             THEN [Catch(s, t) EXCEPT !.doomed = @ \/ (abortish /\ s.tc > 0)]
        ELSE IF abortish THEN Abort(TsqlRollbackAll(s))
        \* The engine rolls back an uncommittable transaction at batch end.
        ELSE IF k = "throw" THEN Abort(IF s.doomed THEN TsqlRollbackAll(s) ELSE s)
        ELSE [s EXCEPT !.raised = TRUE]

\* PostgreSQL.  An exception block is a subtransaction: its changes are
\* rolled back and its handler runs.  Otherwise the whole transaction is
\* aborted: the caller's transaction is doomed, a top-level CALL's
\* transaction is rolled back (earlier COMMITs stay).
PgFail(s, k) ==
    LET e == Innermost(s, LAMBDA i : s.stack[i].kind = "exc")
    IN  IF e > 0 /\ k # "uncatchable"
        THEN Restore(Catch(s, e), s.stack[e].snap)
        ELSE Abort(EndTxn([s EXCEPT !.open = {}, !.doomed = Cs.caller_txn]))

\* Db2 for i.  The innermost eligible handler decides; an unhandled error
\* exits the routine, rolling back the outermost enclosing ATOMIC compound
\* but leaving other pending work for the caller.  Under *NONE nothing is
\* undone (V4 is unverified: this is the documented inference).
Db2Fail(s, k) ==
    LET h       == Innermost(s, LAMBDA i :
                                /\ s.stack[i].kind = "compound"
                                /\ s.stack[i].handler \in {"continue", "exit", "undo"}
                                /\ ~Excluded(s, i))
        canUndo == ~NoCommit
        a       == Outermost(s, LAMBDA i : s.stack[i].atomic)
    IN  IF h > 0
        THEN LET f == s.stack[h] IN
             CASE f.handler = "continue" ->
                    [s EXCEPT !.stack = Append(@, Frame(f.onerr, "handler", f, f.snap, h))]
               [] f.handler = "exit" -> Catch(s, h)
               [] f.handler = "undo" ->
                    IF canUndo THEN Restore(Catch(s, h), f.snap) ELSE Catch(s, h)
        ELSE Abort(IF a > 0 /\ canUndo THEN Restore(s, s.stack[a].snap) ELSE s)

Fail(s, k) ==
    LET s1 == [s EXCEPT !.lastErr = TRUE, !.lastNF = FALSE]
    IN  CASE D = "tsql"     -> TsqlFail(s1, k)
          [] D = "postgres" -> PgFail(s1, IF k = "throw" THEN "stmt" ELSE k)
          [] D = "db2i"     -> Db2Fail(s1, IF k = "throw" THEN "stmt" ELSE k)

-----------------------------------------------------------------------------
(* End of the routine.                                                     *)

Finish(s) ==
    CASE D = "tsql" ->
           \* Error 266: @@TRANCOUNT at exit differs from entry.
           [s EXCEPT !.raised = @ \/ s.tc # InitTc, !.done = TRUE]
      [] D = "postgres" ->
           \* A top-level CALL commits its transaction when it returns.
           IF Cs.caller_txn \/ s.doomed THEN [s EXCEPT !.done = TRUE]
           ELSE [s EXCEPT !.durable = @ \cup s.open, !.open = {}, !.done = TRUE]
      [] D = "db2i" ->
           \* COMMIT ON RETURN NO (V6): pending work is left to the caller.
           [s EXCEPT !.done = TRUE]

-----------------------------------------------------------------------------
(* Statements.                                                             *)

InExc(s)    == \E i \in 1..Len(s.stack) : s.stack[i].kind = "exc"
InAtomic(s) == \E i \in 1..Len(s.stack) : s.stack[i].atomic

Write(s, w) ==
    LET k == Fault(w.id)
        \* IMPLICIT_TRANSACTIONS: any DML opens a transaction, also one that
        \* matches no row or fails.
        s0 == IF D = "tsql" /\ Cs.implicit_transactions /\ s.tc = 0
              THEN [s EXCEPT !.tc = 1] ELSE s
    IN  IF D = "tsql" /\ s.doomed THEN Fail(s, "stmt")          \* error 3930
        ELSE IF k = "ok" THEN Ok(Put(s0, Tags(w)))
        ELSE IF k = "notfound" THEN [Ok(s0) EXCEPT !.lastNF = TRUE]
        ELSE LET partial == w.rows > 1 /\ D = "db2i" /\ (NoCommit \/ w.notatomic)
             IN  Fail(IF partial THEN Put(s0, {w.id \o ".1"}) ELSE s0, k)  \* V2

\* PostgreSQL: COMMIT/ROLLBACK only in a top-level CALL chain and not inside
\* an exception block (V17).
PgTxnCtlAllowed(s) == ~Cs.caller_txn /\ ~InExc(s)

Commit(s) ==
    CASE D = "tsql" ->
           IF s.doomed \/ s.tc = 0 THEN Fail(s, "stmt")          \* 3930 / 3902
           ELSE IF s.tc > 1 THEN Ok([s EXCEPT !.tc = @ - 1])
           ELSE Ok(EndTxn([s EXCEPT !.durable = @ \cup s.open, !.open = {}, !.tc = 0]))
      [] D = "postgres" ->
           IF ~PgTxnCtlAllowed(s) THEN Fail(s, "stmt")           \* 2D000
           ELSE Ok(EndTxn([s EXCEPT !.durable = @ \cup s.open, !.open = {}]))
      [] D = "db2i" ->
           IF InAtomic(s) THEN Assert(FALSE, "COMMIT inside ATOMIC")
           ELSE Ok(EndTxn([s EXCEPT !.durable = @ \cup s.open, !.open = {}]))

Rollback(s) ==
    CASE D = "tsql" ->
           IF s.tc = 0 THEN Fail(s, "stmt")                      \* 3903
           ELSE Ok(TsqlRollbackAll(s))
      [] D = "postgres" ->
           IF ~PgTxnCtlAllowed(s) THEN Fail(s, "stmt")
           ELSE Ok(EndTxn([s EXCEPT !.open = {}, !.doomed = FALSE]))
      [] D = "db2i" ->
           IF InAtomic(s) THEN Assert(FALSE, "ROLLBACK inside ATOMIC")
           ELSE Ok(EndTxn([s EXCEPT !.open = {}]))

SetSp(s, name) ==
    Ok([s EXCEPT !.sp = [x \in DOMAIN @ \cup {name} |-> IF x = name THEN s.open ELSE @[x]]])

Savepoint(s, name) ==
    CASE D = "tsql" ->
           IF s.tc = 0 \/ s.doomed THEN Fail(s, "stmt")          \* 628 / 3931
           ELSE SetSp(s, name)
      [] D = "postgres" -> Assert(FALSE, "PL/pgSQL has no SAVEPOINT")
      [] D = "db2i" ->
           IF NoCommit THEN Fail(s, "stmt")                      \* V14
           ELSE SetSp(s, name)

RollbackTo(s, name) ==
    IF D = "postgres" THEN Assert(FALSE, "PL/pgSQL has no ROLLBACK TO")
    ELSE IF name \notin DOMAIN s.sp \/ (D = "tsql" /\ (s.tc = 0 \/ s.doomed)) \/ NoCommit
         THEN Fail(s, "stmt")
         ELSE Ok(Restore(s, s.sp[name]))

Exec(s, x) ==
    CASE x.op = "write"       -> Write(s, x)
      [] x.op = "begin"       -> Ok([s EXCEPT !.tc = @ + 1])
      [] x.op = "commit"      -> Commit(s)
      [] x.op = "rollback"    -> Rollback(s)
      [] x.op = "savepoint"   -> Savepoint(s, x.id)
      [] x.op = "rollback_to" -> RollbackTo(s, x.id)
      [] x.op = "throw"       -> Fail(s, "throw")
      [] x.op = "raiserror"   -> Fail(s, "raiserror")
      [] x.op = "notice"      -> s       \* RAISE NOTICE leaves FOUND alone
      [] x.op = "onerror" ->
           IF ~s.lastErr THEN s
           ELSE LET s1 == IF x.rollback THEN Rollback(s) ELSE s
                IN  IF s1.done THEN s1 ELSE Finish([s1 EXCEPT !.code = x.n])
      [] x.op = "onnotfound" ->
           IF s.lastNF THEN Finish([s EXCEPT !.code = x.n]) ELSE s
      [] x.op = "return"      -> Finish([s EXCEPT !.code = x.n])
      [] x.op = "block"       ->
           [s EXCEPT !.stack = Append(@, Frame(x.body, x.kind, x, s.open, 0))]

Step(s) ==
    LET n == Len(s.stack)
        f == s.stack[n]
    IN  IF f.pc > Len(f.body)
        THEN IF n = 1 THEN Finish(s) ELSE [s EXCEPT !.stack = SubSeq(@, 1, n - 1)]
        ELSE Exec([s EXCEPT !.stack[n].pc = @ + 1], f.body[f.pc])

-----------------------------------------------------------------------------

Plans(cs) ==
    IF Explore THEN [cs.writes -> FaultKinds(cs.dialect) \cup {"ok"}] ELSE {cs.faults}

Init ==
    /\ c \in CaseIds
    /\ plan \in Plans(Cases[c])
    /\ st = [stack   |-> <<Frame(Cases[c].program, "routine", RoutineBlk, InitOpen, 0)>>,
             durable |-> {},
             open    |-> InitOpen,
             tc      |-> InitTc,
             doomed  |-> FALSE,
             lastErr |-> FALSE,
             lastNF  |-> FALSE,
             raised  |-> FALSE,
             aborted |-> FALSE,
             code    |-> 0,
             sp      |-> NoSp,
             done    |-> FALSE]

Next == ~st.done /\ st' = Step(st) /\ UNCHANGED <<c, plan>>

Done == st.done /\ UNCHANGED vars

Spec == Init /\ [][Next \/ Done]_vars /\ WF_vars(Next)

-----------------------------------------------------------------------------
(* Case check (Explore = FALSE).                                           *)

Outcome == [durable |-> st.durable, open |-> st.open, raised |-> st.raised,
            aborted |-> st.aborted, code |-> st.code, tc |-> st.tc,
            doomed |-> st.doomed]

CaseHolds ==
    st.done => (Outcome = Cs.expect
                \/ Print(<<"case", c, "got", Outcome, "expected", Cs.expect>>, FALSE))

-----------------------------------------------------------------------------
(* Safety (both modes).                                                    *)

Written == st.durable \cup st.open

\* Db2 for i under *NONE has no transaction: nothing is ever pending.
NoCommitHasNoTxn == NoCommit => st.open = {}

\* T-SQL: with no open transaction nothing is pending.
TsqlNoTxnNoPending == D = "tsql" => (st.tc = 0 => st.open = {})

\* A statement is applied in part only on Db2 for i, under *NONE or with
\* NOT ATOMIC (V2).
StatementAtomic ==
    \A m \in Cs.multi :
        ((m \o ".1") \in Written) # ((m \o ".2") \in Written)
            => D = "db2i" /\ (NoCommit \/ m \in Cs.notatomic)

\* PostgreSQL: a routine in a caller's transaction never makes anything
\* durable, and an unhandled error loses all pending work (V17).
PgCallerTxnNothingDurable == D = "postgres" /\ Cs.caller_txn => st.durable = {}
PgAbortLosesPending       == D = "postgres" /\ st.aborted => st.open = {}

\* Only PostgreSQL leaves the caller with a doomed transaction; T-SQL's
\* uncommittable state can only persist if the routine returned normally.
DoomedOnlyWhenExpected ==
    st.doomed => (D = "postgres" /\ st.aborted) \/ (D = "tsql" /\ ~st.aborted)

AbortRaises == st.aborted => st.raised

\* A savepoint never holds work that is no longer pending.
SavepointsPending == \A n \in DOMAIN st.sp : st.sp[n] \subseteq st.open

Safety ==
    /\ NoCommitHasNoTxn
    /\ TsqlNoTxnNoPending
    /\ StatementAtomic
    /\ PgCallerTxnNothingDurable
    /\ PgAbortLosesPending
    /\ DoomedOnlyWhenExpected
    /\ AbortRaises
    /\ SavepointsPending

\* Committed work is never undone.
DurableMonotone == [][st.durable \subseteq st'.durable]_vars

\* Every routine terminates.
Terminates == <>st.done

=============================================================================
