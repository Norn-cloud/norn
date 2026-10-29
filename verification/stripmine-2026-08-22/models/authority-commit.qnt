module authority_commit {
  var ownerVersion: int
  var effectCount: int
  var eventCount: int
  var resultStored: bool
  var storedInput: int
  var capability: bool
  var policyVersion: int
  var committedAuthorized: bool
  var committedPolicyVersion: int

  var lastNoOp: bool
  var beforeOwnerVersion: int
  var beforeEffectCount: int
  var beforeEventCount: int
  var beforeResultStored: bool
  var beforeStoredInput: int
  var beforeCapability: bool
  var beforePolicyVersion: int
  var beforeCommittedAuthorized: bool
  var beforeCommittedPolicyVersion: int

  action init = all {
    ownerVersion' = 0,
    effectCount' = 0,
    eventCount' = 0,
    resultStored' = false,
    storedInput' = 0,
    capability' = true,
    policyVersion' = 0,
    committedAuthorized' = false,
    committedPolicyVersion' = 0,
    lastNoOp' = false,
    beforeOwnerVersion' = 0,
    beforeEffectCount' = 0,
    beforeEventCount' = 0,
    beforeResultStored' = false,
    beforeStoredInput' = 0,
    beforeCapability' = true,
    beforePolicyVersion' = 0,
    beforeCommittedAuthorized' = false,
    beforeCommittedPolicyVersion' = 0,
  }

  action preserveBefore = all {
    beforeOwnerVersion' = beforeOwnerVersion,
    beforeEffectCount' = beforeEffectCount,
    beforeEventCount' = beforeEventCount,
    beforeResultStored' = beforeResultStored,
    beforeStoredInput' = beforeStoredInput,
    beforeCapability' = beforeCapability,
    beforePolicyVersion' = beforePolicyVersion,
    beforeCommittedAuthorized' = beforeCommittedAuthorized,
    beforeCommittedPolicyVersion' = beforeCommittedPolicyVersion,
  }

  action commit(expectedVersion: int, observedPolicyVersion: int, input: int): bool = all {
    expectedVersion == ownerVersion,
    observedPolicyVersion == policyVersion,
    capability,
    not(resultStored),
    input > 0,
    ownerVersion' = ownerVersion + 1,
    effectCount' = effectCount + 1,
    eventCount' = eventCount + 1,
    resultStored' = true,
    storedInput' = input,
    capability' = capability,
    policyVersion' = policyVersion,
    committedAuthorized' = capability,
    committedPolicyVersion' = policyVersion,
    lastNoOp' = false,
    preserveBefore,
  }

  action revoke = all {
    capability,
    ownerVersion' = ownerVersion,
    effectCount' = effectCount,
    eventCount' = eventCount,
    resultStored' = resultStored,
    storedInput' = storedInput,
    capability' = false,
    policyVersion' = policyVersion + 1,
    committedAuthorized' = committedAuthorized,
    committedPolicyVersion' = committedPolicyVersion,
    lastNoOp' = false,
    preserveBefore,
  }

  action grant = all {
    not(capability),
    ownerVersion' = ownerVersion,
    effectCount' = effectCount,
    eventCount' = eventCount,
    resultStored' = resultStored,
    storedInput' = storedInput,
    capability' = true,
    policyVersion' = policyVersion + 1,
    committedAuthorized' = committedAuthorized,
    committedPolicyVersion' = committedPolicyVersion,
    lastNoOp' = false,
    preserveBefore,
  }

  action noOpSnapshot = all {
    ownerVersion' = ownerVersion,
    effectCount' = effectCount,
    eventCount' = eventCount,
    resultStored' = resultStored,
    storedInput' = storedInput,
    capability' = capability,
    policyVersion' = policyVersion,
    committedAuthorized' = committedAuthorized,
    committedPolicyVersion' = committedPolicyVersion,
    lastNoOp' = true,
    beforeOwnerVersion' = ownerVersion,
    beforeEffectCount' = effectCount,
    beforeEventCount' = eventCount,
    beforeResultStored' = resultStored,
    beforeStoredInput' = storedInput,
    beforeCapability' = capability,
    beforePolicyVersion' = policyVersion,
    beforeCommittedAuthorized' = committedAuthorized,
    beforeCommittedPolicyVersion' = committedPolicyVersion,
  }

  action rejectStaleOwner(expectedVersion: int): bool = all {
    expectedVersion != ownerVersion,
    noOpSnapshot,
  }

  action rejectStalePolicy(observedPolicyVersion: int): bool = all {
    observedPolicyVersion != policyVersion,
    noOpSnapshot,
  }

  action rejectUnauthorized = all {
    not(capability),
    noOpSnapshot,
  }

  action retrySame(input: int): bool = all {
    resultStored,
    input == storedInput,
    noOpSnapshot,
  }

  action rejectConflictingRetry(input: int): bool = all {
    resultStored,
    input != storedInput,
    noOpSnapshot,
  }

  action step = nondet expectedVersion = 0.to(4).oneOf()
    nondet observedPolicyVersion = 0.to(4).oneOf()
    nondet input = 1.to(2).oneOf()
    any {
      commit(expectedVersion, observedPolicyVersion, input),
      revoke,
      grant,
      rejectStaleOwner(expectedVersion),
      rejectStalePolicy(observedPolicyVersion),
      rejectUnauthorized,
      retrySame(input),
      rejectConflictingRetry(input),
    }

  def typeOk = all {
    ownerVersion >= 0,
    effectCount >= 0,
    eventCount >= 0,
    policyVersion >= 0,
    storedInput >= 0,
    committedPolicyVersion >= 0,
  }

  def atomicCommit = all {
    effectCount == eventCount,
    effectCount == (if (resultStored) 1 else 0),
    ownerVersion == effectCount,
  }

  def idempotentAtMostOnce = effectCount <= 1

  def acceptedOnlyWithCurrentCapability =
    if (resultStored) committedAuthorized else not(committedAuthorized)

  def committedPolicyWasPinned =
    if (resultStored) committedPolicyVersion <= policyVersion else true

  def noOpPreservesCanonical = if (lastNoOp)
    all {
      beforeOwnerVersion == ownerVersion,
      beforeEffectCount == effectCount,
      beforeEventCount == eventCount,
      beforeResultStored == resultStored,
      beforeStoredInput == storedInput,
      beforeCapability == capability,
      beforePolicyVersion == policyVersion,
      beforeCommittedAuthorized == committedAuthorized,
      beforeCommittedPolicyVersion == committedPolicyVersion,
    }
    else true
}
