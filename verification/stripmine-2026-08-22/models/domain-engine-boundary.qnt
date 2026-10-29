module domain_engine_boundary {
  // 0 = generic ActionIR, 1..5 = inventory/accounting/compliance/settlement/tax.
  // The bounded model deliberately abstracts each domain to one immutable
  // authority tag and checks only routing/commit ownership, not domain logic.
  var genericEffects: int
  var domainEffects: int
  var lastSource: int
  var lastOwner: int
  var lastCommitted: bool
  var beforeGenericEffects: int
  var beforeDomainEffects: int

  action init = all {
    genericEffects' = 0,
    domainEffects' = 0,
    lastSource' = 0,
    lastOwner' = 0,
    lastCommitted' = false,
    beforeGenericEffects' = 0,
    beforeDomainEffects' = 0,
  }

  def admitted(source: int, owner: int): bool =
    (source == 0 and owner == 0) or
    (source > 0 and source == owner)

  action attempt(source: int, owner: int): bool = all {
    source >= 0,
    source <= 5,
    owner >= 0,
    owner <= 5,
    beforeGenericEffects' = genericEffects,
    beforeDomainEffects' = domainEffects,
    lastSource' = source,
    lastOwner' = owner,
    genericEffects' = genericEffects +
      (if (admitted(source, owner) and owner == 0) 1 else 0),
    domainEffects' = domainEffects +
      (if (admitted(source, owner) and owner > 0) 1 else 0),
    lastCommitted' = admitted(source, owner),
  }

  action step =
    nondet source = 0.to(5).oneOf()
    nondet owner = 0.to(5).oneOf()
    attempt(source, owner)

  def typeOk = all {
    genericEffects >= 0,
    domainEffects >= 0,
    lastSource >= 0,
    lastSource <= 5,
    lastOwner >= 0,
    lastOwner <= 5,
  }

  def genericNeverCommitsDomainOwned =
    not(lastCommitted and lastSource == 0 and lastOwner > 0)

  def domainNeverCommitsGenericOwned =
    not(lastCommitted and lastSource > 0 and lastOwner == 0)

  def domainCommitMatchesExactOwner =
    if (lastCommitted and lastOwner > 0) lastSource == lastOwner else true

  def rejectionIsNoOp =
    if (not(lastCommitted))
      all {
        genericEffects == beforeGenericEffects,
        domainEffects == beforeDomainEffects,
      }
    else true
}
