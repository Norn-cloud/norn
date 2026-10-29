import Std

namespace Norn.Verification.OntologyAuthority

/-!
Reference semantics for the ontology/authority type boundary. OWL inference,
SHACL conformance, and reviewed identity mappings may contribute evidence to a
proposal, but only a current scoped capability checked by a typed transition
kernel is authority. This theorem does not establish correspondence to the Rust
runtime; the executable ontology validators and API tests remain separate
evidence.
-/

inductive Evidence where
  | owlInference
  | shaclConformance
  | reviewedIdentityMapping
  | currentScopedCapability
  deriving DecidableEq

def authorizesCommit : Evidence → Bool
  | .currentScopedCapability => true
  | .owlInference | .shaclConformance | .reviewedIdentityMapping => false

theorem semantic_evidence_never_authorizes
    (evidence : Evidence)
    (semantic : evidence ≠ .currentScopedCapability) :
    authorizesCommit evidence = false := by
  cases evidence <;> simp_all [authorizesCommit]

theorem authority_requires_current_scoped_capability
    (evidence : Evidence)
    (authorized : authorizesCommit evidence = true) :
    evidence = .currentScopedCapability := by
  cases evidence <;> simp_all [authorizesCommit]

/-! RelationIR authority reference semantics. The executable Rust validator is separate evidence;
these definitions make the exact-kind, exact-active-version, endpoint, and cardinality obligations
reviewable without pretending that this file proves storage correspondence. -/

inductive Cardinality where
  | oneToOne
  | oneToMany
  | manyToOne
  | manyToMany
  deriving DecidableEq

structure RelationDecl where
  artifactId : String
  fromKind : String
  toKind : String
  cardinality : Cardinality

structure ObjectRef where
  objectId : String
  objectKind : String

structure RelationRef where
  relationId : String
  relationKind : String
  fromObjectId : String
  toObjectId : String

def exactActiveArtifact
    (pinnedArtifactId pinnedVersion activeArtifactId activeVersion : String) : Bool :=
  pinnedArtifactId == activeArtifactId && pinnedVersion == activeVersion

def exactRelationBinding
    (decl : RelationDecl)
    (relation : RelationRef)
    (fromObject toObject : ObjectRef) : Bool :=
  relation.relationKind == decl.artifactId &&
  relation.fromObjectId == fromObject.objectId &&
  relation.toObjectId == toObject.objectId &&
  fromObject.objectKind == decl.fromKind &&
  toObject.objectKind == decl.toKind

def uniqueFrom : Cardinality → Bool
  | .oneToOne | .manyToOne => true
  | .oneToMany | .manyToMany => false

def uniqueTo : Cardinality → Bool
  | .oneToOne | .oneToMany => true
  | .manyToOne | .manyToMany => false

def cardinalityConflict
    (cardinality : Cardinality)
    (candidate existing : RelationRef) : Bool :=
  (uniqueFrom cardinality && candidate.fromObjectId == existing.fromObjectId) ||
  (uniqueTo cardinality && candidate.toObjectId == existing.toObjectId)

def conflictsOtherIdentity
    (cardinality : Cardinality)
    (candidate existing : RelationRef) : Bool :=
  candidate.relationId != existing.relationId &&
  cardinalityConflict cardinality candidate existing

theorem superseded_artifact_version_rejects
    (artifactId pinnedVersion activeVersion : String)
    (superseded : pinnedVersion ≠ activeVersion) :
    exactActiveArtifact artifactId pinnedVersion artifactId activeVersion = false := by
  simp [exactActiveArtifact, superseded]

theorem wrong_from_endpoint_kind_rejects
    (decl : RelationDecl)
    (relation : RelationRef)
    (fromObject toObject : ObjectRef)
    (wrongKind : fromObject.objectKind ≠ decl.fromKind) :
    exactRelationBinding decl relation fromObject toObject = false := by
  simp [exactRelationBinding, wrongKind]

theorem relation_kind_alias_rejects
    (decl : RelationDecl)
    (relation : RelationRef)
    (fromObject toObject : ObjectRef)
    (wrongKind : relation.relationKind ≠ decl.artifactId) :
    exactRelationBinding decl relation fromObject toObject = false := by
  simp [exactRelationBinding, wrongKind]

theorem one_to_many_duplicate_target_conflicts
    (candidate existing : RelationRef)
    (duplicateTarget : candidate.toObjectId = existing.toObjectId) :
    cardinalityConflict .oneToMany candidate existing = true := by
  simp [cardinalityConflict, uniqueFrom, uniqueTo, duplicateTarget]

theorem many_to_one_duplicate_source_conflicts
    (candidate existing : RelationRef)
    (duplicateSource : candidate.fromObjectId = existing.fromObjectId) :
    cardinalityConflict .manyToOne candidate existing = true := by
  simp [cardinalityConflict, uniqueFrom, uniqueTo, duplicateSource]

theorem many_to_many_never_conflicts
    (candidate existing : RelationRef) :
    cardinalityConflict .manyToMany candidate existing = false := by
  simp [cardinalityConflict, uniqueFrom, uniqueTo]

theorem same_immutable_relation_identity_is_not_counted_twice
    (cardinality : Cardinality)
    (candidate existing : RelationRef)
    (sameIdentity : candidate.relationId = existing.relationId) :
    conflictsOtherIdentity cardinality candidate existing = false := by
  simp [conflictsOtherIdentity, sameIdentity]

end Norn.Verification.OntologyAuthority
