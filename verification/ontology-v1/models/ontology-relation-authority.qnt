module ontology_relation_authority {
  // Finite abstraction of one scoped RelationIR authority with two candidate
  // relation identities. Cardinality 0/1/2/3 denotes one-to-one,
  // one-to-many, many-to-one, and many-to-many.
  var activeVersion: int
  var cardinality: int
  var firstExists: bool
  var secondExists: bool
  var firstFrom: int
  var firstTo: int
  var secondFrom: int
  var secondTo: int
  var acceptedPinnedVersion: int
  var acceptedActiveVersion: int
  var endpointKindsValid: bool
  var lastAttemptRejected: bool
  var beforeActiveVersion: int
  var beforeCardinality: int
  var beforeFirstExists: bool
  var beforeSecondExists: bool
  var beforeFirstFrom: int
  var beforeFirstTo: int
  var beforeSecondFrom: int
  var beforeSecondTo: int
  var beforeAcceptedPinnedVersion: int
  var beforeAcceptedActiveVersion: int
  var beforeEndpointKindsValid: bool

  action init = all {
    activeVersion' = 1,
    cardinality' = 0,
    firstExists' = false,
    secondExists' = false,
    firstFrom' = 0,
    firstTo' = 0,
    secondFrom' = 0,
    secondTo' = 0,
    acceptedPinnedVersion' = 0,
    acceptedActiveVersion' = 0,
    endpointKindsValid' = true,
    lastAttemptRejected' = false,
    beforeActiveVersion' = 1,
    beforeCardinality' = 0,
    beforeFirstExists' = false,
    beforeSecondExists' = false,
    beforeFirstFrom' = 0,
    beforeFirstTo' = 0,
    beforeSecondFrom' = 0,
    beforeSecondTo' = 0,
    beforeAcceptedPinnedVersion' = 0,
    beforeAcceptedActiveVersion' = 0,
    beforeEndpointKindsValid' = true,
  }

  action preserveRejectionWitness = all {
    lastAttemptRejected' = false,
    beforeActiveVersion' = beforeActiveVersion,
    beforeCardinality' = beforeCardinality,
    beforeFirstExists' = beforeFirstExists,
    beforeSecondExists' = beforeSecondExists,
    beforeFirstFrom' = beforeFirstFrom,
    beforeFirstTo' = beforeFirstTo,
    beforeSecondFrom' = beforeSecondFrom,
    beforeSecondTo' = beforeSecondTo,
    beforeAcceptedPinnedVersion' = beforeAcceptedPinnedVersion,
    beforeAcceptedActiveVersion' = beforeAcceptedActiveVersion,
    beforeEndpointKindsValid' = beforeEndpointKindsValid,
  }

  action captureRejectedState = all {
    lastAttemptRejected' = true,
    beforeActiveVersion' = activeVersion,
    beforeCardinality' = cardinality,
    beforeFirstExists' = firstExists,
    beforeSecondExists' = secondExists,
    beforeFirstFrom' = firstFrom,
    beforeFirstTo' = firstTo,
    beforeSecondFrom' = secondFrom,
    beforeSecondTo' = secondTo,
    beforeAcceptedPinnedVersion' = acceptedPinnedVersion,
    beforeAcceptedActiveVersion' = acceptedActiveVersion,
    beforeEndpointKindsValid' = endpointKindsValid,
  }

  action commitFirst = all {
    not(firstExists),
    activeVersion == 1,
    firstExists' = true,
    firstFrom' = 1,
    firstTo' = 1,
    acceptedPinnedVersion' = 1,
    acceptedActiveVersion' = activeVersion,
    endpointKindsValid' = true,
    activeVersion' = activeVersion,
    cardinality' = cardinality,
    secondExists' = secondExists,
    secondFrom' = secondFrom,
    secondTo' = secondTo,
    preserveRejectionWitness,
  }

  def conflicts(fromId: int, toId: int): bool = if (cardinality == 0) {
    (firstExists and (firstFrom == fromId or firstTo == toId)) or
    (secondExists and (secondFrom == fromId or secondTo == toId))
  } else if (cardinality == 1) {
    (firstExists and firstTo == toId) or (secondExists and secondTo == toId)
  } else if (cardinality == 2) {
    (firstExists and firstFrom == fromId) or (secondExists and secondFrom == fromId)
  } else {
    false
  }

  action commitNonConflictingSecond = all {
    firstExists,
    not(secondExists),
    activeVersion == 1,
    not(conflicts(2, 2)),
    secondExists' = true,
    secondFrom' = 2,
    secondTo' = 2,
    acceptedPinnedVersion' = 1,
    acceptedActiveVersion' = activeVersion,
    endpointKindsValid' = true,
    activeVersion' = activeVersion,
    cardinality' = cardinality,
    firstExists' = firstExists,
    firstFrom' = firstFrom,
    firstTo' = firstTo,
    preserveRejectionWitness,
  }

  action chooseCardinality(nextCardinality: int): bool = all {
    not(firstExists),
    cardinality' = nextCardinality,
    activeVersion' = activeVersion,
    firstExists' = firstExists,
    secondExists' = secondExists,
    firstFrom' = firstFrom,
    firstTo' = firstTo,
    secondFrom' = secondFrom,
    secondTo' = secondTo,
    acceptedPinnedVersion' = acceptedPinnedVersion,
    acceptedActiveVersion' = acceptedActiveVersion,
    endpointKindsValid' = endpointKindsValid,
    preserveRejectionWitness,
  }

  action supersedeDefinition = all {
    activeVersion == 1,
    activeVersion' = 2,
    cardinality' = cardinality,
    firstExists' = firstExists,
    secondExists' = secondExists,
    firstFrom' = firstFrom,
    firstTo' = firstTo,
    secondFrom' = secondFrom,
    secondTo' = secondTo,
    acceptedPinnedVersion' = acceptedPinnedVersion,
    acceptedActiveVersion' = acceptedActiveVersion,
    endpointKindsValid' = endpointKindsValid,
    preserveRejectionWitness,
  }

  action rejectStaleVersion = all {
    activeVersion' = activeVersion,
    cardinality' = cardinality,
    firstExists' = firstExists,
    secondExists' = secondExists,
    firstFrom' = firstFrom,
    firstTo' = firstTo,
    secondFrom' = secondFrom,
    secondTo' = secondTo,
    acceptedPinnedVersion' = acceptedPinnedVersion,
    acceptedActiveVersion' = acceptedActiveVersion,
    endpointKindsValid' = endpointKindsValid,
    captureRejectedState,
  }

  action rejectWrongEndpointKind = rejectStaleVersion
  action rejectCardinalityConflict = rejectStaleVersion
  action stutter = all {
    activeVersion' = activeVersion,
    cardinality' = cardinality,
    firstExists' = firstExists,
    secondExists' = secondExists,
    firstFrom' = firstFrom,
    firstTo' = firstTo,
    secondFrom' = secondFrom,
    secondTo' = secondTo,
    acceptedPinnedVersion' = acceptedPinnedVersion,
    acceptedActiveVersion' = acceptedActiveVersion,
    endpointKindsValid' = endpointKindsValid,
    preserveRejectionWitness,
  }

  action step = nondet nextCardinality = 0.to(3).oneOf()
    any {
      chooseCardinality(nextCardinality),
      commitFirst,
      commitNonConflictingSecond,
      supersedeDefinition,
      rejectStaleVersion,
      rejectWrongEndpointKind,
      rejectCardinalityConflict,
      stutter,
    }

  def typeOk = all {
    activeVersion >= 1,
    activeVersion <= 2,
    cardinality >= 0,
    cardinality <= 3,
    firstFrom >= 0,
    firstFrom <= 2,
    firstTo >= 0,
    firstTo <= 2,
    secondFrom >= 0,
    secondFrom <= 2,
    secondTo >= 0,
    secondTo <= 2,
  }

  def acceptedVersionWasExact = acceptedPinnedVersion == acceptedActiveVersion
  def acceptedEndpointsHadExactKinds = endpointKindsValid

  def cardinalityPreserved = not(all { firstExists, secondExists }) or
    if (cardinality == 0) all { firstFrom != secondFrom, firstTo != secondTo }
    else if (cardinality == 1) firstTo != secondTo
    else if (cardinality == 2) firstFrom != secondFrom
    else true

  def rejectedAttemptIsNoOp = if (lastAttemptRejected)
    all {
      beforeActiveVersion == activeVersion,
      beforeCardinality == cardinality,
      beforeFirstExists == firstExists,
      beforeSecondExists == secondExists,
      beforeFirstFrom == firstFrom,
      beforeFirstTo == firstTo,
      beforeSecondFrom == secondFrom,
      beforeSecondTo == secondTo,
      beforeAcceptedPinnedVersion == acceptedPinnedVersion,
      beforeAcceptedActiveVersion == acceptedActiveVersion,
      beforeEndpointKindsValid == endpointKindsValid,
    }
    else true
}
