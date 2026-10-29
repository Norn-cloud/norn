module artifact_promotion {
  // Hashes are abstract positive integers. Zero means that no such artifact or
  // checker result exists. The catalog authority bit corresponds to an
  // Approved/Active authority-bearing artifact that runtime paths may consume.
  var proposalHash: int
  var validatedHash: int
  var approvalHash: int
  var catalogHash: int
  var catalogValidatedHash: int
  var catalogApprovalHash: int
  var catalogAuthority: bool
  var planApplied: bool

  action init = all {
    proposalHash' = 0,
    validatedHash' = 0,
    approvalHash' = 0,
    catalogHash' = 0,
    catalogValidatedHash' = 0,
    catalogApprovalHash' = 0,
    catalogAuthority' = false,
    planApplied' = false,
  }

  action propose(hash: int): bool = all {
    hash > 0,
    proposalHash' = hash,
    validatedHash' = 0,
    approvalHash' = 0,
    catalogHash' = catalogHash,
    catalogValidatedHash' = catalogValidatedHash,
    catalogApprovalHash' = catalogApprovalHash,
    catalogAuthority' = catalogAuthority,
    planApplied' = planApplied,
  }

  action validate = all {
    proposalHash > 0,
    proposalHash' = proposalHash,
    validatedHash' = proposalHash,
    approvalHash' = approvalHash,
    catalogHash' = catalogHash,
    catalogValidatedHash' = catalogValidatedHash,
    catalogApprovalHash' = catalogApprovalHash,
    catalogAuthority' = catalogAuthority,
    planApplied' = planApplied,
  }

  action approve = all {
    proposalHash > 0,
    validatedHash == proposalHash,
    proposalHash' = proposalHash,
    validatedHash' = validatedHash,
    approvalHash' = proposalHash,
    catalogHash' = catalogHash,
    catalogValidatedHash' = catalogValidatedHash,
    catalogApprovalHash' = catalogApprovalHash,
    catalogAuthority' = catalogAuthority,
    planApplied' = planApplied,
  }

  action apply = all {
    proposalHash > 0,
    validatedHash == proposalHash,
    approvalHash == proposalHash,
    proposalHash' = proposalHash,
    validatedHash' = validatedHash,
    approvalHash' = approvalHash,
    catalogHash' = proposalHash,
    catalogValidatedHash' = validatedHash,
    catalogApprovalHash' = approvalHash,
    catalogAuthority' = true,
    planApplied' = true,
  }

  // Models an attacker changing the proposed artifact after validation or
  // approval. Exact-hash checks prevent the modified value from being applied.
  action tamper(hash: int): bool = all {
    hash > 0,
    hash != proposalHash,
    proposalHash' = hash,
    validatedHash' = validatedHash,
    approvalHash' = approvalHash,
    catalogHash' = catalogHash,
    catalogValidatedHash' = catalogValidatedHash,
    catalogApprovalHash' = catalogApprovalHash,
    catalogAuthority' = catalogAuthority,
    planApplied' = planApplied,
  }

  // This is the direct PUT attempt closed in the production API. It is a
  // canonical no-op even when the caller possesses an artifact-write grant.
  action rejectDirectAuthorityWrite = all {
    proposalHash' = proposalHash,
    validatedHash' = validatedHash,
    approvalHash' = approvalHash,
    catalogHash' = catalogHash,
    catalogValidatedHash' = catalogValidatedHash,
    catalogApprovalHash' = catalogApprovalHash,
    catalogAuthority' = catalogAuthority,
    planApplied' = planApplied,
  }

  action step = nondet hash = 1.to(2).oneOf()
    any {
      propose(hash),
      validate,
      approve,
      apply,
      tamper(hash),
      rejectDirectAuthorityWrite,
    }

  def typeOk = all {
    proposalHash >= 0,
    validatedHash >= 0,
    approvalHash >= 0,
    catalogHash >= 0,
    catalogValidatedHash >= 0,
    catalogApprovalHash >= 0,
  }

  def authorityRequiresExactAppliedPlan = if (catalogAuthority)
    all {
      planApplied,
      catalogHash > 0,
      catalogHash == catalogValidatedHash,
      catalogHash == catalogApprovalHash,
    }
    else true
}
