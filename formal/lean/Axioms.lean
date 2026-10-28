import Sqldiff.Gate
import Sqldiff.Mirror.Check
import Sqldiff.Core.Fuel

-- CI compares this output against the allowlist in check_axioms.py.
#print axioms Sqldiff.Gate.check_sound
#print axioms Sqldiff.Mirror.check_sound
#print axioms Sqldiff.Core.refines_rename
#print axioms Sqldiff.Core.obs_mono
