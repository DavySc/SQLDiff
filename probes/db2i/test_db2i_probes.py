"""Tests for db2i_probes.py: python3 -m unittest discover -s probes/db2i

No test here touches a real IBM i: result rows are made up from the outcomes in
probes.toml, only to exercise the judging. They are never written to runs/.
"""

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import db2i_probes as dp  # noqa: E402


def value_for(pattern):
    """A value that matches `pattern`."""
    if pattern in ("*", "null"):
        return None
    if pattern.startswith("contains:"):
        return pattern[len("contains:"):]
    if pattern.startswith("!"):
        return "42999"
    for op, delta in ((">=", 0), ("<=", 0), (">", 1), ("<", -1), ("=", 0)):
        if pattern.startswith(op):
            return int(pattern[len(op):]) + delta
    return pattern


def rows_for(defs, pick):
    """Result rows in which each probe has the outcome `pick(probe)` returns."""
    rows, seq = [], 0

    def add(**kw):
        nonlocal seq
        seq += 1
        row = {"seq": seq, "state": None, "code": None, "o_state": None, "o_code": None,
               "n1": None, "n2": None, "text": None, "msg": None}
        row.update(kw)
        rows.append(row)

    created = set()
    for pr in defs["probe"]:
        o = pick(pr)
        when = o["when"] if o else {}
        if "procedure" in pr and pr["procedure"] not in created:
            created.add(pr["procedure"])
            add(probe=f"PROC:{pr['procedure']}", step="create", state=value_for(when.get("create", "00000")))
        add(probe=pr["id"], step="call",
            state=value_for(when.get("call", "00000")),
            o_state=value_for(when.get("state", "00000")),
            o_code=0,
            n1=value_for(when["n1"]) if "n1" in when else None,
            n2=value_for(when["n2"]) if "n2" in when else None,
            text=value_for(when["text"]) if "text" in when else None)
    return rows


def first(verdicts):
    def pick(pr):
        for o in pr["outcome"]:
            if o["verdict"] in verdicts:
                return o
        return pr["outcome"][0]
    return pick


def as_lines(rows):
    def f(v):
        return "~" if v is None else str(v)
    return "\n".join(
        dp.LINE_TAG + "|".join(f(r[k]) for k in ("seq", "probe", "step", "state", "code", "o_state",
                                                 "o_code", "n1", "n2", "text", "msg"))
        for r in rows
    )


class Definitions(unittest.TestCase):
    def test_check_passes(self):
        self.assertEqual(dp.cmd_check(None), 0)

    def test_every_consistent_outcome_is_reachable(self):
        # An earlier outcome must not shadow a later one for the values it names.
        defs = dp.load_probes()
        for pr in defs["probe"]:
            for o in pr["outcome"]:
                rows = rows_for(defs, lambda p, pr=pr, o=o: o if p is pr else None)
                j = dp.judge(pr, rows)
                self.assertEqual(j["outcome"], o["label"], f"{pr['id']}: {o['label']} is shadowed by {j['outcome']}")

    def test_literal_pieces_rebuild_the_text(self):
        text = "SET X = 'it''s' || 'a ''b'''; " * 20
        pieces = dp.sql_literal_pieces(text, 30)
        self.assertTrue(all(len(p) <= 30 for p in pieces))
        rebuilt = "".join(p[1:-1].replace("''", "'") for p in pieces)
        self.assertEqual(rebuilt, text)


class Results(unittest.TestCase):
    def test_parse_plain_and_csv(self):
        plain = "SQLDFP|1|3|V9-INT-EMPTY|call|00000|0|22018|-420|~|~||Character in CAST argument not valid."
        csv = 'x,"SQLDFP|1|4|V9-INT-BLANK|call|00000|0|22018|-420|~|~|~|He said ""no"" / twice"'
        rows = dp.parse_results("header\n" + plain + "\n" + csv + "\n")
        self.assertEqual(len(rows), 2)
        self.assertEqual(rows[0]["o_state"], "22018")
        self.assertEqual(rows[0]["o_code"], -420)
        self.assertIsNone(rows[0]["n1"])
        self.assertEqual(rows[0]["text"], "")
        self.assertEqual(rows[1]["msg"], 'He said "no" / twice')
        self.assertIsNone(rows[1]["text"])

    def test_all_as_expected(self):
        defs, facts = dp.load_probes(), dp.load_facts()
        ev = dp.evaluate(defs, facts, rows_for(defs, first({"consistent"})))
        self.assertTrue(ev["harness_ok"])
        st = {k: v["status"] for k, v in ev["facts"].items()}
        for fid in ("V4", "V9", "V14", "V15", "V16", "V19", "V3", "V7"):
            self.assertEqual(st[fid], "confirmed", (fid, ev["facts"][fid]))
        self.assertEqual(st["V5"], "observed")

    def test_contradiction_and_correction(self):
        defs, facts = dp.load_probes(), dp.load_facts()

        def pick(pr):
            if pr["id"] == "V9-INT-EMPTY":
                return next(o for o in pr["outcome"] if o["label"] == "zero")
            if pr["id"] == "V14-SAVEPOINT-NONE":
                return next(o for o in pr["outcome"] if o["label"] == "accepted")
            if pr["id"] == "V4-ROLLBACK-NONE":
                return next(o for o in pr["outcome"] if o["verdict"] == "refines")
            return first({"consistent"})(pr)

        ev = dp.evaluate(defs, facts, rows_for(defs, pick))
        self.assertEqual(ev["facts"]["V9"]["status"], "contradicted")
        self.assertIn("V9-INT-EMPTY", ev["facts"]["V9"]["why"])
        self.assertEqual(ev["facts"]["V14"]["status"], "contradicted")
        self.assertEqual(ev["facts"]["V4"]["status"], "corrected")

    def test_failed_control_makes_the_fact_inconclusive(self):
        defs, facts = dp.load_probes(), dp.load_facts()
        rows = rows_for(defs, first({"consistent"}))
        for r in rows:
            if r["probe"] == "V14-SAVEPOINT-CS":
                r["n1"] = 2
        ev = dp.evaluate(defs, facts, rows)
        self.assertEqual(ev["facts"]["V14"]["status"], "inconclusive")
        self.assertEqual(ev["facts"]["V9"]["status"], "confirmed")

    def test_failed_harness_makes_everything_inconclusive(self):
        defs, facts = dp.load_probes(), dp.load_facts()
        rows = rows_for(defs, first({"consistent"}))
        for r in rows:
            if r["probe"] == "H-COMMIT-CONTROL":
                r["n2"] = 1
        ev = dp.evaluate(defs, facts, rows)
        self.assertFalse(ev["harness_ok"])
        self.assertEqual({v["status"] for v in ev["facts"].values()}, {"inconclusive"})

    def test_missing_rows_are_not_a_verdict(self):
        defs, facts = dp.load_probes(), dp.load_facts()
        rows = [r for r in rows_for(defs, first({"consistent"})) if r["probe"] != "V19-ORDER-ASC"]
        ev = dp.evaluate(defs, facts, rows)
        self.assertEqual(ev["facts"]["V19"]["status"], "inconclusive")

    def test_ingest_writes_run_and_status(self):
        defs = dp.load_probes()
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            saved = dp.RUNS, dp.STATUS
            dp.RUNS, dp.STATUS = tmp / "runs", tmp / "status.json"
            try:
                rows = rows_for(defs, first({"consistent"}))
                rows.insert(0, {"seq": 0, "probe": "META-FORMAT", "step": "call", "state": "00000", "code": 1,
                                "o_state": None, "o_code": None, "n1": None, "n2": None,
                                "text": dp.probes_hash(), "msg": "probe set"})
                src = tmp / "results.csv"
                src.write_text(as_lines(rows))
                code = dp.main(["ingest", str(src), "--interface", "acs", "--run", "synthetic-a"])
                self.assertEqual(code, 0)
                status = json.loads(dp.STATUS.read_text())
                self.assertEqual(status["facts"]["V9"]["status"], "confirmed")
                self.assertEqual(status["runs"][0]["probe_set"], dp.probes_hash())
                report = (dp.RUNS / "synthetic-a.md").read_text()
                self.assertIn("ASM-FACT-V9", report)
                # A second run that disagrees makes the fact mixed.
                for r in rows:
                    if r["probe"] == "V9-INT-EMPTY":
                        r["o_state"], r["n1"] = "00000", 0
                src.write_text(as_lines(rows))
                dp.main(["ingest", str(src), "--interface", "acs", "--run", "synthetic-b"])
                status = json.loads(dp.STATUS.read_text())
                self.assertEqual(status["facts"]["V9"]["status"], "mixed")
            finally:
                dp.RUNS, dp.STATUS = saved


if __name__ == "__main__":
    unittest.main()
