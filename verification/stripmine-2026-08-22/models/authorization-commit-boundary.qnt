module authorization_commit_boundary {
  var policyVersion: int
  var authorizationEpoch: int
  var resourceVersion: int
  var policyActive: bool
  var grantActive: bool
  var logicalTime: int
  var committed: bool
  var commitCount: int

  var committedWellFormed: bool
  var committedIdentityBound: bool
  var committedScopeBound: bool
  var committedOperationBound: bool
  var committedPolicyIdentityBound: bool
  var committedPolicyAllows: bool
  var committedPolicyActive: bool
  var committedGrantActive: bool
  var committedExpectedPolicyVersion: int
  var committedGrantPolicyVersion: int
  var committedEvidencePolicyVersion: int
  var committedExpectedResourceVersion: int
  var committedCurrentResourceVersion: int
  var committedEvidenceResourceVersion: int
  var committedObservedEpoch: int
  var committedGrantEpoch: int
  var committedEvidenceEpoch: int
  var committedAt: int
  var committedPolicyEvaluatedAt: int
  var committedNotBefore: int
  var committedHasExpiry: bool
  var committedExpiresAt: int

  var lastRejected: bool
  var beforeCommitted: bool
  var beforeCommitCount: int

  action init = all {
    policyVersion' = 1,
    authorizationEpoch' = 1,
    resourceVersion' = 1,
    policyActive' = true,
    grantActive' = true,
    logicalTime' = 1,
    committed' = false,
    commitCount' = 0,
    committedWellFormed' = false,
    committedIdentityBound' = false,
    committedScopeBound' = false,
    committedOperationBound' = false,
    committedPolicyIdentityBound' = false,
    committedPolicyAllows' = false,
    committedPolicyActive' = false,
    committedGrantActive' = false,
    committedExpectedPolicyVersion' = 0,
    committedGrantPolicyVersion' = 0,
    committedEvidencePolicyVersion' = 0,
    committedExpectedResourceVersion' = 0,
    committedCurrentResourceVersion' = 0,
    committedEvidenceResourceVersion' = 0,
    committedObservedEpoch' = 0,
    committedGrantEpoch' = 0,
    committedEvidenceEpoch' = 0,
    committedAt' = 0,
    committedPolicyEvaluatedAt' = 0,
    committedNotBefore' = 0,
    committedHasExpiry' = false,
    committedExpiresAt' = 0,
    lastRejected' = false,
    beforeCommitted' = false,
    beforeCommitCount' = 0,
  }

  def authorizes(
    wellFormed: bool,
    identityBound: bool,
    scopeBound: bool,
    operationBound: bool,
    policyIdentityBound: bool,
    policyAllows: bool,
    expectedPolicyVersion: int,
    grantPolicyVersion: int,
    evidencePolicyVersion: int,
    expectedResourceVersion: int,
    evidenceResourceVersion: int,
    observedEpoch: int,
    grantEpoch: int,
    evidenceEpoch: int,
    notBefore: int,
    hasExpiry: bool,
    expiresAt: int,
    policyEvaluatedAt: int,
  ): bool = all {
    wellFormed,
    identityBound,
    scopeBound,
    operationBound,
    policyIdentityBound,
    policyAllows,
    policyActive,
    grantActive,
    expectedPolicyVersion == policyVersion,
    grantPolicyVersion == policyVersion,
    evidencePolicyVersion == policyVersion,
    expectedResourceVersion == resourceVersion,
    evidenceResourceVersion == resourceVersion,
    observedEpoch == authorizationEpoch,
    grantEpoch == authorizationEpoch,
    evidenceEpoch == authorizationEpoch,
    notBefore <= logicalTime,
    policyEvaluatedAt == logicalTime,
    if (hasExpiry) all { notBefore < expiresAt, logicalTime < expiresAt } else true,
  }

  action preserveAuthority = all {
    committed' = committed,
    commitCount' = commitCount,
    committedWellFormed' = committedWellFormed,
    committedIdentityBound' = committedIdentityBound,
    committedScopeBound' = committedScopeBound,
    committedOperationBound' = committedOperationBound,
    committedPolicyIdentityBound' = committedPolicyIdentityBound,
    committedPolicyAllows' = committedPolicyAllows,
    committedPolicyActive' = committedPolicyActive,
    committedGrantActive' = committedGrantActive,
    committedExpectedPolicyVersion' = committedExpectedPolicyVersion,
    committedGrantPolicyVersion' = committedGrantPolicyVersion,
    committedEvidencePolicyVersion' = committedEvidencePolicyVersion,
    committedExpectedResourceVersion' = committedExpectedResourceVersion,
    committedCurrentResourceVersion' = committedCurrentResourceVersion,
    committedEvidenceResourceVersion' = committedEvidenceResourceVersion,
    committedObservedEpoch' = committedObservedEpoch,
    committedGrantEpoch' = committedGrantEpoch,
    committedEvidenceEpoch' = committedEvidenceEpoch,
    committedAt' = committedAt,
    committedPolicyEvaluatedAt' = committedPolicyEvaluatedAt,
    committedNotBefore' = committedNotBefore,
    committedHasExpiry' = committedHasExpiry,
    committedExpiresAt' = committedExpiresAt,
  }

  action commit(
    wellFormed: bool,
    identityBound: bool,
    scopeBound: bool,
    operationBound: bool,
    policyIdentityBound: bool,
    policyAllows: bool,
    expectedPolicyVersion: int,
    grantPolicyVersion: int,
    evidencePolicyVersion: int,
    expectedResourceVersion: int,
    evidenceResourceVersion: int,
    observedEpoch: int,
    grantEpoch: int,
    evidenceEpoch: int,
    notBefore: int,
    hasExpiry: bool,
    expiresAt: int,
    policyEvaluatedAt: int,
  ): bool = all {
    not(committed),
    authorizes(
      wellFormed,
      identityBound,
      scopeBound,
      operationBound,
      policyIdentityBound,
      policyAllows,
      expectedPolicyVersion,
      grantPolicyVersion,
      evidencePolicyVersion,
      expectedResourceVersion,
      evidenceResourceVersion,
      observedEpoch,
      grantEpoch,
      evidenceEpoch,
      notBefore,
      hasExpiry,
      expiresAt,
      policyEvaluatedAt,
    ),
    policyVersion' = policyVersion,
    authorizationEpoch' = authorizationEpoch,
    resourceVersion' = resourceVersion,
    policyActive' = policyActive,
    grantActive' = grantActive,
    logicalTime' = logicalTime,
    committed' = true,
    commitCount' = commitCount + 1,
    committedWellFormed' = wellFormed,
    committedIdentityBound' = identityBound,
    committedScopeBound' = scopeBound,
    committedOperationBound' = operationBound,
    committedPolicyIdentityBound' = policyIdentityBound,
    committedPolicyAllows' = policyAllows,
    committedPolicyActive' = policyActive,
    committedGrantActive' = grantActive,
    committedExpectedPolicyVersion' = expectedPolicyVersion,
    committedGrantPolicyVersion' = grantPolicyVersion,
    committedEvidencePolicyVersion' = evidencePolicyVersion,
    committedExpectedResourceVersion' = expectedResourceVersion,
    committedCurrentResourceVersion' = resourceVersion,
    committedEvidenceResourceVersion' = evidenceResourceVersion,
    committedObservedEpoch' = observedEpoch,
    committedGrantEpoch' = grantEpoch,
    committedEvidenceEpoch' = evidenceEpoch,
    committedAt' = logicalTime,
    committedPolicyEvaluatedAt' = policyEvaluatedAt,
    committedNotBefore' = notBefore,
    committedHasExpiry' = hasExpiry,
    committedExpiresAt' = expiresAt,
    lastRejected' = false,
    beforeCommitted' = committed,
    beforeCommitCount' = commitCount,
  }

  action reject(
    wellFormed: bool,
    identityBound: bool,
    scopeBound: bool,
    operationBound: bool,
    policyIdentityBound: bool,
    policyAllows: bool,
    expectedPolicyVersion: int,
    grantPolicyVersion: int,
    evidencePolicyVersion: int,
    expectedResourceVersion: int,
    evidenceResourceVersion: int,
    observedEpoch: int,
    grantEpoch: int,
    evidenceEpoch: int,
    notBefore: int,
    hasExpiry: bool,
    expiresAt: int,
    policyEvaluatedAt: int,
  ): bool = all {
    not(authorizes(
      wellFormed,
      identityBound,
      scopeBound,
      operationBound,
      policyIdentityBound,
      policyAllows,
      expectedPolicyVersion,
      grantPolicyVersion,
      evidencePolicyVersion,
      expectedResourceVersion,
      evidenceResourceVersion,
      observedEpoch,
      grantEpoch,
      evidenceEpoch,
      notBefore,
      hasExpiry,
      expiresAt,
      policyEvaluatedAt,
    )),
    policyVersion' = policyVersion,
    authorizationEpoch' = authorizationEpoch,
    resourceVersion' = resourceVersion,
    policyActive' = policyActive,
    grantActive' = grantActive,
    logicalTime' = logicalTime,
    preserveAuthority,
    lastRejected' = true,
    beforeCommitted' = committed,
    beforeCommitCount' = commitCount,
  }

  action revoke = all {
    grantActive,
    policyVersion' = policyVersion,
    authorizationEpoch' = authorizationEpoch + 1,
    resourceVersion' = resourceVersion,
    policyActive' = policyActive,
    grantActive' = false,
    logicalTime' = logicalTime,
    preserveAuthority,
    lastRejected' = false,
    beforeCommitted' = committed,
    beforeCommitCount' = commitCount,
  }

  action grant = all {
    not(grantActive),
    policyVersion' = policyVersion,
    authorizationEpoch' = authorizationEpoch + 1,
    resourceVersion' = resourceVersion,
    policyActive' = policyActive,
    grantActive' = true,
    logicalTime' = logicalTime,
    preserveAuthority,
    lastRejected' = false,
    beforeCommitted' = committed,
    beforeCommitCount' = commitCount,
  }

  action changePolicy = all {
    policyVersion' = policyVersion + 1,
    authorizationEpoch' = authorizationEpoch + 1,
    resourceVersion' = resourceVersion,
    policyActive' = policyActive,
    grantActive' = grantActive,
    logicalTime' = logicalTime,
    preserveAuthority,
    lastRejected' = false,
    beforeCommitted' = committed,
    beforeCommitCount' = commitCount,
  }

  action changeResource = all {
    policyVersion' = policyVersion,
    authorizationEpoch' = authorizationEpoch,
    resourceVersion' = resourceVersion + 1,
    policyActive' = policyActive,
    grantActive' = grantActive,
    logicalTime' = logicalTime,
    preserveAuthority,
    lastRejected' = false,
    beforeCommitted' = committed,
    beforeCommitCount' = commitCount,
  }

  action deactivatePolicy = all {
    policyActive,
    policyVersion' = policyVersion + 1,
    authorizationEpoch' = authorizationEpoch + 1,
    resourceVersion' = resourceVersion,
    policyActive' = false,
    grantActive' = grantActive,
    logicalTime' = logicalTime,
    preserveAuthority,
    lastRejected' = false,
    beforeCommitted' = committed,
    beforeCommitCount' = commitCount,
  }

  action tick = all {
    logicalTime < 4,
    policyVersion' = policyVersion,
    authorizationEpoch' = authorizationEpoch,
    resourceVersion' = resourceVersion,
    policyActive' = policyActive,
    grantActive' = grantActive,
    logicalTime' = logicalTime + 1,
    preserveAuthority,
    lastRejected' = false,
    beforeCommitted' = committed,
    beforeCommitCount' = commitCount,
  }

  action step =
    nondet wellFormed = Set(true, false).oneOf()
    nondet identityBound = Set(true, false).oneOf()
    nondet scopeBound = Set(true, false).oneOf()
    nondet operationBound = Set(true, false).oneOf()
    nondet policyIdentityBound = Set(true, false).oneOf()
    nondet policyAllows = Set(true, false).oneOf()
    nondet expectedPolicyVersion = 1.to(4).oneOf()
    nondet grantPolicyVersion = 1.to(4).oneOf()
    nondet evidencePolicyVersion = 1.to(4).oneOf()
    nondet expectedResourceVersion = 1.to(4).oneOf()
    nondet evidenceResourceVersion = 1.to(4).oneOf()
    nondet observedEpoch = 1.to(5).oneOf()
    nondet grantEpoch = 1.to(5).oneOf()
    nondet evidenceEpoch = 1.to(5).oneOf()
    nondet notBefore = 0.to(4).oneOf()
    nondet hasExpiry = Set(true, false).oneOf()
    nondet expiresAt = 1.to(5).oneOf()
    nondet policyEvaluatedAt = 1.to(4).oneOf()
    any {
      commit(
        wellFormed, identityBound, scopeBound, operationBound, policyIdentityBound,
        policyAllows, expectedPolicyVersion, grantPolicyVersion, evidencePolicyVersion,
        expectedResourceVersion, evidenceResourceVersion,
        observedEpoch, grantEpoch, evidenceEpoch, notBefore, hasExpiry, expiresAt,
        policyEvaluatedAt,
      ),
      reject(
        wellFormed, identityBound, scopeBound, operationBound, policyIdentityBound,
        policyAllows, expectedPolicyVersion, grantPolicyVersion, evidencePolicyVersion,
        expectedResourceVersion, evidenceResourceVersion,
        observedEpoch, grantEpoch, evidenceEpoch, notBefore, hasExpiry, expiresAt,
        policyEvaluatedAt,
      ),
      revoke,
      grant,
      changePolicy,
      changeResource,
      deactivatePolicy,
      tick,
    }

  def typeOk = all {
    policyVersion >= 1,
    authorizationEpoch >= 1,
    resourceVersion >= 1,
    logicalTime >= 1,
    commitCount >= 0,
  }

  def atMostOnce = commitCount <= 1

  def acceptedOnlyWithExactBindings = if (committed) all {
    committedWellFormed,
    committedIdentityBound,
    committedScopeBound,
    committedOperationBound,
    committedPolicyIdentityBound,
    committedPolicyAllows,
    committedPolicyActive,
    committedGrantActive,
  } else true

  def acceptedOnlyWithFreshVersions = if (committed) all {
    committedExpectedPolicyVersion == committedGrantPolicyVersion,
    committedExpectedPolicyVersion == committedEvidencePolicyVersion,
    committedExpectedResourceVersion == committedCurrentResourceVersion,
    committedExpectedResourceVersion == committedEvidenceResourceVersion,
    committedObservedEpoch == committedGrantEpoch,
    committedObservedEpoch == committedEvidenceEpoch,
  } else true

  def acceptedOnlyWithinValidityWindow = if (committed) all {
    committedPolicyEvaluatedAt == committedAt,
    committedNotBefore <= committedAt,
    if (committedHasExpiry) committedAt < committedExpiresAt else true,
  } else true

  def rejectionPreservesCommitAuthority = if (lastRejected) all {
    beforeCommitted == committed,
    beforeCommitCount == commitCount,
  } else true
}
