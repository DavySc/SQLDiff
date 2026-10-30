import Sqldiff.Gate
import Sqldiff.Mirror.Check
import Sqldiff.Core.Fuel
import Sqldiff.Core.OpFree
import Sqldiff.Core.Data
import Sqldiff.Core.Handlers
import Sqldiff.Core.Fidelity

-- CI compares this output against the allowlist in check_axioms.py.
#print axioms Sqldiff.Gate.check_sound
#print axioms Sqldiff.Mirror.check_sound
#print axioms Sqldiff.Mirror.check_sound_c2a
#print axioms Sqldiff.Core.refines_rename
#print axioms Sqldiff.Core.obs_mono
#print axioms Sqldiff.Core.Routine.obs_opFree
#print axioms Sqldiff.Core.exec_error_atomic
#print axioms Sqldiff.Core.insertRows_map
#print axioms Sqldiff.Core.updateRows_map
#print axioms Sqldiff.Core.deleteRows_map
#print axioms Sqldiff.Mirror.mkRtrim_eval
-- C4: handled errors.
#print axioms Sqldiff.Core.exec_undoSafe
#print axioms Sqldiff.Core.exec_settle
#print axioms Sqldiff.Core.exec_settleTop
#print axioms Sqldiff.Core.Routine.obs_settleTop
#print axioms Sqldiff.Core.refines_settleTop_rename
-- C3b: queries.
#print axioms Sqldiff.Core.keptTuples_map
#print axioms Sqldiff.Core.blockRows_map
#print axioms Sqldiff.Core.keptTuples_congr
#print axioms Sqldiff.Core.pickRows_map
#print axioms Sqldiff.Core.pickRows_rename
#print axioms Sqldiff.Core.exec_setDb
-- C3d: the key closure keeps at most one tuple.
#print axioms Sqldiff.Core.kept_length_le_one
-- C2c: typed values.
#print axioms Sqldiff.Mirror.mkToDate_dateAdd_eval
#print axioms Sqldiff.Core.Calendar.dayOf_addToTs
