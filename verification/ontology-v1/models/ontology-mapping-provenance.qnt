module ontology_mapping_provenance {
  // Finite two-record abstraction. Mapping 1 begins as the sole active mapping
  // for term/source identity 1/1. A valid replacement creates mapping 2,
  // directs mapping 1 to it, and leaves mapping 2 terminal/active. Invalid
  // replacement attempts are explicit no-ops.
  var oldSuccessor: int
  var replacementExists: bool
  var replacementSuccessor: int
  var replacementTerm: int
  var replacementSource: int
  var replacementConfidence: int
  var replacementReviewed: bool
  var lastAttemptRejected: bool
  var beforeOldSuccessor: int
  var beforeReplacementExists: bool
  var beforeReplacementSuccessor: int
  var beforeReplacementTerm: int
  var beforeReplacementSource: int
  var beforeReplacementConfidence: int
  var beforeReplacementReviewed: bool

  action init = all {
    oldSuccessor' = 0,
    replacementExists' = false,
    replacementSuccessor' = 0,
    replacementTerm' = 1,
    replacementSource' = 1,
    replacementConfidence' = 0,
    replacementReviewed' = false,
    lastAttemptRejected' = false,
    beforeOldSuccessor' = 0,
    beforeReplacementExists' = false,
    beforeReplacementSuccessor' = 0,
    beforeReplacementTerm' = 1,
    beforeReplacementSource' = 1,
    beforeReplacementConfidence' = 0,
    beforeReplacementReviewed' = false,
  }

  action preserveRejectionWitness = all {
    lastAttemptRejected' = false,
    beforeOldSuccessor' = beforeOldSuccessor,
    beforeReplacementExists' = beforeReplacementExists,
    beforeReplacementSuccessor' = beforeReplacementSuccessor,
    beforeReplacementTerm' = beforeReplacementTerm,
    beforeReplacementSource' = beforeReplacementSource,
    beforeReplacementConfidence' = beforeReplacementConfidence,
    beforeReplacementReviewed' = beforeReplacementReviewed,
  }

  action captureRejectedState = all {
    lastAttemptRejected' = true,
    beforeOldSuccessor' = oldSuccessor,
    beforeReplacementExists' = replacementExists,
    beforeReplacementSuccessor' = replacementSuccessor,
    beforeReplacementTerm' = replacementTerm,
    beforeReplacementSource' = replacementSource,
    beforeReplacementConfidence' = replacementConfidence,
    beforeReplacementReviewed' = replacementReviewed,
  }

  action addValidReplacement = all {
    oldSuccessor == 0,
    not(replacementExists),
    oldSuccessor' = 2,
    replacementExists' = true,
    replacementSuccessor' = 0,
    replacementTerm' = 1,
    replacementSource' = 1,
    replacementConfidence' = 10000,
    replacementReviewed' = true,
    preserveRejectionWitness,
  }

  action rejectSelfReplacement = all {
    oldSuccessor' = oldSuccessor,
    replacementExists' = replacementExists,
    replacementSuccessor' = replacementSuccessor,
    replacementTerm' = replacementTerm,
    replacementSource' = replacementSource,
    replacementConfidence' = replacementConfidence,
    replacementReviewed' = replacementReviewed,
    captureRejectedState,
  }

  action rejectCrossIdentityReplacement = rejectSelfReplacement
  action rejectChainedReplacement = rejectSelfReplacement
  action rejectUnreviewedOrUnbounded = rejectSelfReplacement
  action stutter = all {
    oldSuccessor' = oldSuccessor,
    replacementExists' = replacementExists,
    replacementSuccessor' = replacementSuccessor,
    replacementTerm' = replacementTerm,
    replacementSource' = replacementSource,
    replacementConfidence' = replacementConfidence,
    replacementReviewed' = replacementReviewed,
    preserveRejectionWitness,
  }

  action step = any {
    addValidReplacement,
    rejectSelfReplacement,
    rejectCrossIdentityReplacement,
    rejectChainedReplacement,
    rejectUnreviewedOrUnbounded,
    stutter,
  }

  def typeOk = all {
    oldSuccessor >= 0,
    oldSuccessor <= 2,
    replacementSuccessor >= 0,
    replacementSuccessor <= 2,
    replacementConfidence >= 0,
    replacementConfidence <= 10000,
  }

  def replacementIsDirect = not(replacementExists) or all {
    oldSuccessor == 2,
    replacementSuccessor == 0,
  }

  def replacementPreservesIdentity = not(replacementExists) or all {
    replacementTerm == 1,
    replacementSource == 1,
  }

  def replacementHasReviewedBoundedProvenance = not(replacementExists) or all {
    replacementConfidence >= 0,
    replacementConfidence <= 10000,
    replacementReviewed,
  }

  def exactlyOneActivePerIdentity = if (replacementExists) {
    oldSuccessor != 0 and replacementSuccessor == 0
  } else {
    oldSuccessor == 0
  }

  def rejectedAttemptIsNoOp = if (lastAttemptRejected)
    all {
      beforeOldSuccessor == oldSuccessor,
      beforeReplacementExists == replacementExists,
      beforeReplacementSuccessor == replacementSuccessor,
      beforeReplacementTerm == replacementTerm,
      beforeReplacementSource == replacementSource,
      beforeReplacementConfidence == replacementConfidence,
      beforeReplacementReviewed == replacementReviewed,
    }
    else true
}
