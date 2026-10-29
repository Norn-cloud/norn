module quantity_authority {
  type Phase = Empty | Posted | Reversed

  var phase: Phase
  var ownerVersion: int
  var sourceBalance: int
  var destinationBalance: int
  var transactionCount: int
  var eventCount: int
  var outboxCount: int
  var receiptCount: int
  var storedPostingInput: int
  var storedReversalInput: int
  var lastNoOp: bool
  var beforePhase: Phase
  var beforeOwnerVersion: int
  var beforeSourceBalance: int
  var beforeDestinationBalance: int
  var beforeTransactionCount: int
  var beforeEventCount: int
  var beforeOutboxCount: int
  var beforeReceiptCount: int

  action init = all {
    phase' = Empty,
    ownerVersion' = 0,
    sourceBalance' = 0,
    destinationBalance' = 0,
    transactionCount' = 0,
    eventCount' = 0,
    outboxCount' = 0,
    receiptCount' = 0,
    storedPostingInput' = 0,
    storedReversalInput' = 0,
    lastNoOp' = false,
    beforePhase' = Empty,
    beforeOwnerVersion' = 0,
    beforeSourceBalance' = 0,
    beforeDestinationBalance' = 0,
    beforeTransactionCount' = 0,
    beforeEventCount' = 0,
    beforeOutboxCount' = 0,
    beforeReceiptCount' = 0,
  }

  action preserveBefore = all {
    beforePhase' = beforePhase,
    beforeOwnerVersion' = beforeOwnerVersion,
    beforeSourceBalance' = beforeSourceBalance,
    beforeDestinationBalance' = beforeDestinationBalance,
    beforeTransactionCount' = beforeTransactionCount,
    beforeEventCount' = beforeEventCount,
    beforeOutboxCount' = beforeOutboxCount,
    beforeReceiptCount' = beforeReceiptCount,
  }

  action post(
    expectedVersion: int,
    postingInput: int,
    amount: int,
    checkedSeal: bool,
    scopeMatches: bool,
    certificateCoversEvaluation: bool,
  ): bool = all {
    phase == Empty,
    expectedVersion == ownerVersion,
    postingInput > 0,
    amount > 0,
    checkedSeal,
    scopeMatches,
    certificateCoversEvaluation,
    phase' = Posted,
    ownerVersion' = ownerVersion + 1,
    sourceBalance' = sourceBalance - amount,
    destinationBalance' = destinationBalance + amount,
    transactionCount' = transactionCount + 1,
    eventCount' = eventCount + 1,
    outboxCount' = outboxCount + 1,
    receiptCount' = receiptCount + 1,
    storedPostingInput' = postingInput,
    storedReversalInput' = storedReversalInput,
    lastNoOp' = false,
    preserveBefore,
  }

  action reverse(
    expectedVersion: int,
    reversalInput: int,
  ): bool = all {
    phase == Posted,
    expectedVersion == ownerVersion,
    reversalInput > 0,
    phase' = Reversed,
    ownerVersion' = ownerVersion + 1,
    sourceBalance' = 0,
    destinationBalance' = 0,
    transactionCount' = transactionCount + 1,
    eventCount' = eventCount + 1,
    outboxCount' = outboxCount + 1,
    receiptCount' = receiptCount + 1,
    storedPostingInput' = storedPostingInput,
    storedReversalInput' = reversalInput,
    lastNoOp' = false,
    preserveBefore,
  }

  action noOp = all {
    phase' = phase,
    ownerVersion' = ownerVersion,
    sourceBalance' = sourceBalance,
    destinationBalance' = destinationBalance,
    transactionCount' = transactionCount,
    eventCount' = eventCount,
    outboxCount' = outboxCount,
    receiptCount' = receiptCount,
    storedPostingInput' = storedPostingInput,
    storedReversalInput' = storedReversalInput,
    lastNoOp' = true,
    beforePhase' = phase,
    beforeOwnerVersion' = ownerVersion,
    beforeSourceBalance' = sourceBalance,
    beforeDestinationBalance' = destinationBalance,
    beforeTransactionCount' = transactionCount,
    beforeEventCount' = eventCount,
    beforeOutboxCount' = outboxCount,
    beforeReceiptCount' = receiptCount,
  }

  action exactPostingRetry(input: int): bool = all {
    phase != Empty,
    input == storedPostingInput,
    noOp,
  }

  action exactReversalRetry(input: int): bool = all {
    phase == Reversed,
    input == storedReversalInput,
    noOp,
  }

  action conflictingRetry(input: int): bool = all {
    phase != Empty,
    input != storedPostingInput,
    input != storedReversalInput,
    noOp,
  }

  action reject = noOp

  action step = nondet expectedVersion = 0.to(2).oneOf()
    nondet input = 1.to(3).oneOf()
    nondet amount = 1.to(3).oneOf()
    nondet checkedSeal = Set(true, false).oneOf()
    nondet scopeMatches = Set(true, false).oneOf()
    nondet certificateCoversEvaluation = Set(true, false).oneOf()
    any {
      post(
        expectedVersion,
        input,
        amount,
        checkedSeal,
        scopeMatches,
        certificateCoversEvaluation,
      ),
      reverse(expectedVersion, input),
      exactPostingRetry(input),
      exactReversalRetry(input),
      conflictingRetry(input),
      reject,
    }

  def typeOk = all {
    phase.in(Set(Empty, Posted, Reversed)),
    ownerVersion >= 0,
    transactionCount >= 0,
    eventCount >= 0,
    outboxCount >= 0,
    receiptCount >= 0,
  }

  def perUnitLayerConserved = sourceBalance + destinationBalance == 0

  def atomicAuthorityRecords = all {
    ownerVersion == transactionCount,
    transactionCount == eventCount,
    eventCount == outboxCount,
    outboxCount == receiptCount,
  }

  def phaseMatchesCommitCount = all {
    (phase == Empty) == (transactionCount == 0),
    (phase == Posted) == (transactionCount == 1),
    (phase == Reversed) == (transactionCount == 2),
  }

  def reversalRestoresBalances = if (phase == Reversed)
    all {
      sourceBalance == 0,
      destinationBalance == 0,
      storedPostingInput > 0,
      storedReversalInput > 0,
    }
    else true

  def noOpPreservesAuthority = if (lastNoOp)
    all {
      beforePhase == phase,
      beforeOwnerVersion == ownerVersion,
      beforeSourceBalance == sourceBalance,
      beforeDestinationBalance == destinationBalance,
      beforeTransactionCount == transactionCount,
      beforeEventCount == eventCount,
      beforeOutboxCount == outboxCount,
      beforeReceiptCount == receiptCount,
    }
    else true
}
