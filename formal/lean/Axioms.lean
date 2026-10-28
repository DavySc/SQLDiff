import Sqldiff.Gate
import Sqldiff.Mirror.Check
import Sqldiff.Core.Fuel
import Sqldiff.Core.OpFree
import Sqldiff.Core.Data

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
