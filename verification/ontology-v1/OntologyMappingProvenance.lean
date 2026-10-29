import Std

namespace Norn.Verification.OntologyMappingProvenance

/-!
Compact reference semantics for reviewed ontology-mapping provenance.

This file proves properties of the closed mathematical predicate below. It does
not prove correspondence to the Rust, TypeScript, or Python validators and it
does not authenticate reviewer identities or establish that cited evidence is
true. Those remain separately declared trust and implementation obligations.
-/

structure Mapping where
  mappingId : Nat
  termId : Nat
  sourceId : Nat
  confidenceBasisPoints : Nat
  reviewed : Bool
  supersededByMappingId : Option Nat
  deriving DecidableEq

def provenanceValid (mapping : Mapping) : Bool :=
  mapping.confidenceBasisPoints <= 10000 && mapping.reviewed

def directReplacementValid (prior replacement : Mapping) : Bool :=
  provenanceValid prior &&
  provenanceValid replacement &&
  prior.mappingId != replacement.mappingId &&
  prior.supersededByMappingId == some replacement.mappingId &&
  replacement.supersededByMappingId == none &&
  prior.termId == replacement.termId &&
  prior.sourceId == replacement.sourceId

def active (mapping : Mapping) : Bool :=
  mapping.supersededByMappingId == none

theorem accepted_provenance_bounds_confidence
    (mapping : Mapping)
    (accepted : provenanceValid mapping = true) :
    mapping.confidenceBasisPoints <= 10000 := by
  simp_all [provenanceValid]

theorem accepted_provenance_requires_review
    (mapping : Mapping)
    (accepted : provenanceValid mapping = true) :
    mapping.reviewed = true := by
  simp_all [provenanceValid]

theorem direct_replacement_is_not_self
    (prior replacement : Mapping)
    (accepted : directReplacementValid prior replacement = true) :
    prior.mappingId != replacement.mappingId := by
  simp_all [directReplacementValid, provenanceValid]

theorem direct_replacement_preserves_term
    (prior replacement : Mapping)
    (accepted : directReplacementValid prior replacement = true) :
    prior.termId = replacement.termId := by
  simp_all [directReplacementValid, provenanceValid]

theorem direct_replacement_preserves_source
    (prior replacement : Mapping)
    (accepted : directReplacementValid prior replacement = true) :
    prior.sourceId = replacement.sourceId := by
  simp_all [directReplacementValid, provenanceValid]

theorem direct_replacement_is_terminal
    (prior replacement : Mapping)
    (accepted : directReplacementValid prior replacement = true) :
    replacement.supersededByMappingId = none := by
  simp_all [directReplacementValid, provenanceValid]

theorem direct_replacement_leaves_exactly_one_active
    (prior replacement : Mapping)
    (accepted : directReplacementValid prior replacement = true) :
    active prior = false ∧ active replacement = true := by
  simp_all [directReplacementValid, provenanceValid, active]

end Norn.Verification.OntologyMappingProvenance
