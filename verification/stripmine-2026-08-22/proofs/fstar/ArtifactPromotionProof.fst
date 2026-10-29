module ArtifactPromotionProof

open Norn_ir

let authority_bearing (kind: t_ArtifactKind) : bool =
  match kind with
  | ArtifactKind_Entity
  | ArtifactKind_Relation
  | ArtifactKind_Action
  | ArtifactKind_Rule
  | ArtifactKind_Law
  | ArtifactKind_Connector
  | ArtifactKind_Widget
  | ArtifactKind_Customization
  | ArtifactKind_Setting
  | ArtifactKind_BusinessPack
  | ArtifactKind_Policy
  | ArtifactKind_Derived
  | ArtifactKind_Mapping -> true
  | ArtifactKind_SourceSchema -> false

let active_authority_bearing_rejected (kind: t_ArtifactKind)
  : Lemma
    (requires authority_bearing kind)
    (ensures direct_artifact_write_allowed kind ArtifactLifecycle_Active == false)
  = match kind with
    | _ -> ()

let approved_authority_bearing_rejected (kind: t_ArtifactKind)
  : Lemma
    (requires authority_bearing kind)
    (ensures direct_artifact_write_allowed kind ArtifactLifecycle_Approved == false)
  = match kind with
    | _ -> ()

let grants_runtime_authority (lifecycle: t_ArtifactLifecycle) : bool =
  match lifecycle with
  | ArtifactLifecycle_Approved
  | ArtifactLifecycle_Active -> true
  | ArtifactLifecycle_Draft
  | ArtifactLifecycle_Proposed
  | ArtifactLifecycle_Deprecated
  | ArtifactLifecycle_Superseded
  | ArtifactLifecycle_Rejected -> false

let expected_direct_write
  (kind: t_ArtifactKind)
  (lifecycle: t_ArtifactLifecycle)
  : bool =
  not (authority_bearing kind && grants_runtime_authority lifecycle)

/// Exact total truth table for the hax-extracted production predicate over the
/// current closed ArtifactKind (14 variants) and ArtifactLifecycle (7 variants)
/// representations. SourceSchema is evidence-only and is not authority-bearing;
/// Mapping is authority-bearing. A future enum variant must re-extract and
/// re-check this theorem rather than falling outside a precondition.
let direct_artifact_write_truth_table
  (kind: t_ArtifactKind)
  (lifecycle: t_ArtifactLifecycle)
  : Lemma
    (ensures
      direct_artifact_write_allowed kind lifecycle ==
      expected_direct_write kind lifecycle)
  = match kind, lifecycle with
    | _, _ -> ()
