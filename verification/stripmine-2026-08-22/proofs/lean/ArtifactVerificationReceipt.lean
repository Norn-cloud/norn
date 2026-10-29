/-
Compact reference model for Norn's executable-artifact checker receipt.

This proves only the binding rule: execution authority requires a successful receipt for the
exact scope/id/version/hash and the pinned checker profile/version. It does not prove that the
checker establishes arbitrary business semantics or refine the Rust implementation.
-/

namespace Norn.ArtifactVerificationReceipt

structure Artifact where
  scope : String
  artifactId : String
  version : String
  canonicalHash : String
  deriving DecidableEq

structure Receipt where
  scope : String
  artifactId : String
  version : String
  canonicalHash : String
  checkerProfile : String
  checkerVersion : String
  claimsExact : Bool
  succeeded : Bool
  evidenceDigestValid : Bool
  deriving DecidableEq

def trustedProfile : String := "norn.artifact-authority.activation"
def trustedVersion : String := "1"

def receiptMatches (artifact : Artifact) (receipt : Receipt) : Bool :=
  receipt.scope == artifact.scope &&
  receipt.artifactId == artifact.artifactId &&
  receipt.version == artifact.version &&
  receipt.canonicalHash == artifact.canonicalHash &&
  receipt.checkerProfile == trustedProfile &&
  receipt.checkerVersion == trustedVersion &&
  receipt.claimsExact && receipt.succeeded && receipt.evidenceDigestValid

def executable
    (activeVersion : Option String)
    (artifact : Artifact)
    (receipt : Option Receipt) : Bool :=
  activeVersion == some artifact.version &&
  receipt.any (receiptMatches artifact)

theorem missing_receipt_fails_closed
    (activeVersion : Option String)
    (artifact : Artifact) :
    executable activeVersion artifact none = false := by
  simp [executable]

theorem executable_requires_exact_successful_receipt
    (activeVersion : Option String)
    (artifact : Artifact)
    (receipt : Receipt)
    (h : executable activeVersion artifact (some receipt) = true) :
    activeVersion = some artifact.version ∧
    receiptMatches artifact receipt = true := by
  simpa [executable] using h

theorem wrong_checker_version_fails_closed
    (activeVersion : Option String)
    (artifact : Artifact)
    (receipt : Receipt)
    (hWrong : receipt.checkerVersion ≠ trustedVersion) :
    executable activeVersion artifact (some receipt) = false := by
  simp [executable, receiptMatches, hWrong]

theorem wrong_artifact_hash_fails_closed
    (activeVersion : Option String)
    (artifact : Artifact)
    (receipt : Receipt)
    (hWrong : receipt.canonicalHash ≠ artifact.canonicalHash) :
    executable activeVersion artifact (some receipt) = false := by
  simp [executable, receiptMatches, hWrong]

end Norn.ArtifactVerificationReceipt
