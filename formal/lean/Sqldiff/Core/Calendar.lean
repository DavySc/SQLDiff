/-
The proleptic Gregorian calendar of the core's date and timestamp values
(C2c). A date is a number of days since 0001-01-01 (day 0); a timestamp
is a number of microseconds since 0001-01-01T00:00:00. Both vendors keep
their dates in this calendar (Microsoft Learn, "date (Transact-SQL)":
"Gregorian calendar"; PostgreSQL 18 manual, B.6 "History of Units":
"PostgreSQL uses Julian dates for all date/time calculations" with the
Gregorian rules "projected backward"), and both describe months and
years by the same rules: a year of 365 days, 366 when divisible by 4 but
not by 100, or by 400; months of 31, 28 or 29, 31, 30, 31, 30, 31, 31,
30, 31, 30, 31 days.

The conversions between a day number and a civil date are the standard
era-of-400-years algorithm (Howard Hinnant, "chrono-Compatible Low-Level
Date Algorithms"), checked below against the vendors' own values: the
probe eval/values V7 counts 3652058 days from 0001-01-01 to 9999-12-31
on both engines.
-/

namespace Sqldiff.Core.Calendar

/-- Microseconds in a day. -/
def usPerDay : Int := 86400000000

/-- Days from 0001-01-01 to 1970-01-01. -/
def epochShift : Int := 719162

def isLeap (y : Int) : Bool :=
  y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)

def daysInMonth (y m : Int) : Int :=
  if m == 2 then (if isLeap y then 29 else 28)
  else if m == 4 || m == 6 || m == 9 || m == 11 then 30
  else 31

/-- The day number of a civil date `y-m-d` (days since 0001-01-01). The
date need not be valid: an invalid day is read as an offset from the
first of its month (callers check validity with `daysInMonth`). -/
def daysFromCivil (y m d : Int) : Int :=
  let y := if m ≤ 2 then y - 1 else y
  let era := y.fdiv 400
  let yoe := y - era * 400
  let mp := (m + 9).fmod 12
  let doy := (153 * mp + 2).fdiv 5 + d - 1
  let doe := yoe * 365 + yoe.fdiv 4 - yoe.fdiv 100 + doy
  era * 146097 + doe - 719468 + epochShift

/-- The civil date `(y, m, d)` of a day number. -/
def civilFromDays (z : Int) : Int × Int × Int :=
  let z := z - epochShift + 719468
  let era := z.fdiv 146097
  let doe := z - era * 146097
  let yoe := (doe - doe.fdiv 1460 + doe.fdiv 36524 - doe.fdiv 146096).fdiv 365
  let y := yoe + era * 400
  let doy := doe - (365 * yoe + yoe.fdiv 4 - yoe.fdiv 100)
  let mp := (5 * doy + 2).fdiv 153
  let d := doy - (153 * mp + 2).fdiv 5 + 1
  let m := if mp < 10 then mp + 3 else mp - 9
  (if m ≤ 2 then y + 1 else y, m, d)

/-- `y-m-d` is a date of the Gregorian calendar within 0001-01-01 and
9999-12-31 (T-SQL's range for `date` and `datetime2`). -/
def validDate (y m d : Int) : Bool :=
  1 ≤ y && y ≤ 9999 && 1 ≤ m && m ≤ 12 && 1 ≤ d && d ≤ daysInMonth y m

/-- `k` months after day `z`: the same day of the month, or the last day of
the result month when it has fewer days (both vendors' rule for months
and years, eval/values V6). -/
def addMonths (k z : Int) : Int :=
  let (y, m, d) := civilFromDays z
  let total := y * 12 + (m - 1) + k
  let y' := total.fdiv 12
  let m' := total.fmod 12 + 1
  daysFromCivil y' m' (min d (daysInMonth y' m'))

/-- The date of a timestamp (its day number). -/
def dayOf (us : Int) : Int := us.fdiv usPerDay

/-- `k` days, months or years after a timestamp: the time of day is kept. -/
def addToTs (f : Int → Int) (us : Int) : Int :=
  f (dayOf us) * usPerDay + us.fmod usPerDay

/-- A date at midnight, as a timestamp, has that date. -/
theorem dayOf_mul (d : Int) : dayOf (d * usPerDay) = d := by
  unfold dayOf usPerDay
  exact Int.mul_fdiv_cancel d (by decide)

theorem fmod_mul (d : Int) : (d * usPerDay).fmod usPerDay = 0 := by
  unfold usPerDay
  simp [Int.fmod_eq_emod_of_nonneg _ (by decide : (0:Int) ≤ 86400000000), Int.mul_emod_left]

/-- Moving a midnight by whole days and taking its date is moving the date. -/
theorem dayOf_addToTs (f : Int → Int) (d : Int) : dayOf (addToTs f (d * usPerDay)) = f d := by
  unfold addToTs
  rw [dayOf_mul, fmod_mul, Int.add_zero, dayOf_mul]

-- The vendors' own values: V7 counts 3652058 days from 0001-01-01 to
-- 9999-12-31; the Unix epoch; the clamps of V6.
#guard daysFromCivil 1 1 1 = 0
#guard daysFromCivil 9999 12 31 = 3652058
#guard daysFromCivil 1970 1 1 = epochShift
#guard civilFromDays 0 = (1, 1, 1)
#guard civilFromDays 3652058 = (9999, 12, 31)
#guard civilFromDays (daysFromCivil 2024 2 29) = (2024, 2, 29)
#guard civilFromDays (daysFromCivil 1900 3 1) = (1900, 3, 1)
#guard daysFromCivil 2024 3 1 - daysFromCivil 2023 12 31 = 61
#guard daysFromCivil 2024 1 1 - daysFromCivil 1 1 1 = 738885
#guard addMonths 1 (daysFromCivil 2024 1 31) = daysFromCivil 2024 2 29
#guard addMonths 13 (daysFromCivil 2024 1 31) = daysFromCivil 2025 2 28
#guard addMonths (-1) (daysFromCivil 2024 3 31) = daysFromCivil 2024 2 29
#guard addMonths 12 (daysFromCivil 2024 2 29) = daysFromCivil 2025 2 28
#guard addMonths (-12) (daysFromCivil 2024 2 29) = daysFromCivil 2023 2 28
#guard addMonths 48 (daysFromCivil 2024 2 29) = daysFromCivil 2028 2 29
#guard addMonths 1 (daysFromCivil 9999 12 31) = daysFromCivil 10000 1 31
#guard isLeap 2000 && !isLeap 1900 && isLeap 2024 && !isLeap 2023
#guard validDate 2024 2 29 && !validDate 2023 2 29 && !validDate 2024 2 30 && !validDate 0 1 1
#guard addToTs (addMonths 1) (daysFromCivil 2024 1 31 * usPerDay + 37230123456) =
  daysFromCivil 2024 2 29 * usPerDay + 37230123456
#guard dayOf (daysFromCivil 2024 1 1 * usPerDay + usPerDay - 1) = daysFromCivil 2024 1 1

end Sqldiff.Core.Calendar
