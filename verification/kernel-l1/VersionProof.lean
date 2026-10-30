import Version
import Reference

namespace Norn.KernelL1
open Aeneas.Std

/-- Expose successful machine values as mathematical naturals. Fail/diverge
    remain distinguishable from a successful overflow refusal. -/
def projectVersion : Result (Option U64) → Option (Option Nat)
  | .ok value => some (value.map (fun version => version.val))
  | .fail _ => none
  | .div => none

/-- The fresh Aeneas translation of the production Rust file returns exactly
    the reference successor for every machine u64, including exhaustion. -/
theorem version_correspondence (version : U64) :
    projectVersion (norn_kernel_version.checked_next_version version) =
      some (nextVersion version.val) := by
  have spec := U64.checked_add_bv_spec version 1#u64
  cases h : U64.checked_add version 1#u64 with
  | none =>
      simp only [h] at spec
      simp [U64.max, U64.numBits] at spec
      have refused : ¬ version.val < maxVersion := by
        unfold maxVersion
        omega
      simp [norn_kernel_version.checked_next_version, projectVersion, h,
        nextVersion, refused]
  | some next =>
      simp only [h] at spec
      simp [U64.max, U64.numBits] at spec
      have accepted : version.val < maxVersion := by
        unfold maxVersion
        omega
      simp [norn_kernel_version.checked_next_version, projectVersion, h,
        nextVersion, accepted, spec.2.1]

end Norn.KernelL1
#print axioms Norn.KernelL1.version_correspondence
