module quantity_reservation_authority {
  type Phase = Empty | Pending | Posted | Voided | Expired

  var phase: Phase
  var ownerVersion: int
  var reservedSource: int
  var reservedDestination: int
  var postedSource: int
  var postedDestination: int
  var resolutionCount: int
  var eventCount: int
  var outboxCount: int
  var receiptCount: int
  var storedCreateInput: int
  var storedResolutionInput: int
  var expiryAt: int
  var lastNoOp: bool
  var beforePhase: Phase
  var beforeOwnerVersion: int
  var beforeReservedSource: int
  var beforeReservedDestination: int
  var beforePostedSource: int
  var beforePostedDestination: int
  var beforeResolutionCount: int
  var beforeEventCount: int
  var beforeOutboxCount: int
  var beforeReceiptCount: int

  action init = all {
    phase' = Empty,
    ownerVersion' = 0,
    reservedSource' = 0,
    reservedDestination' = 0,
    postedSource' = 0,
    postedDestination' = 0,
    resolutionCount' = 0,
    eventCount' = 0,
    outboxCount' = 0,
    receiptCount' = 0,
    storedCreateInput' = 0,
    storedResolutionInput' = 0,
    expiryAt' = 0,
    lastNoOp' = false,
    beforePhase' = Empty,
    beforeOwnerVersion' = 0,
    beforeReservedSource' = 0,
    beforeReservedDestination' = 0,
    beforePostedSource' = 0,
    beforePostedDestination' = 0,
    beforeResolutionCount' = 0,
    beforeEventCount' = 0,
    beforeOutboxCount' = 0,
    beforeReceiptCount' = 0,
  }

  action preserveBefore = all {
    beforePhase' = beforePhase,
    beforeOwnerVersion' = beforeOwnerVersion,
    beforeReservedSource' = beforeReservedSource,
    beforeReservedDestination' = beforeReservedDestination,
    beforePostedSource' = beforePostedSource,
    beforePostedDestination' = beforePostedDestination,
    beforeResolutionCount' = beforeResolutionCount,
    beforeEventCount' = beforeEventCount,
    beforeOutboxCount' = beforeOutboxCount,
    beforeReceiptCount' = beforeReceiptCount,
  }

  action reserve(expectedVersion: int, input: int, amount: int, expires: int): bool = all {
    phase == Empty,
    expectedVersion == ownerVersion,
    input > 0,
    amount > 0,
    expires > 0,
    phase' = Pending,
    ownerVersion' = ownerVersion + 1,
    reservedSource' = reservedSource - amount,
    reservedDestination' = reservedDestination + amount,
    postedSource' = postedSource,
    postedDestination' = postedDestination,
    resolutionCount' = resolutionCount,
    eventCount' = eventCount + 1,
    outboxCount' = outboxCount + 1,
    receiptCount' = receiptCount + 1,
    storedCreateInput' = input,
    storedResolutionInput' = storedResolutionInput,
    expiryAt' = expires,
    lastNoOp' = false,
    preserveBefore,
  }

  action postAll(expectedVersion: int, input: int, amount: int): bool = all {
    phase == Pending,
    expectedVersion == ownerVersion,
    input > 0,
    amount == reservedDestination,
    phase' = Posted,
    ownerVersion' = ownerVersion + 1,
    reservedSource' = 0,
    reservedDestination' = 0,
    postedSource' = postedSource - amount,
    postedDestination' = postedDestination + amount,
    resolutionCount' = resolutionCount + 1,
    eventCount' = eventCount + 1,
    outboxCount' = outboxCount + 1,
    receiptCount' = receiptCount + 1,
    storedCreateInput' = storedCreateInput,
    storedResolutionInput' = input,
    expiryAt' = expiryAt,
    lastNoOp' = false,
    preserveBefore,
  }

  action void(expectedVersion: int, input: int): bool = all {
    phase == Pending,
    expectedVersion == ownerVersion,
    input > 0,
    phase' = Voided,
    ownerVersion' = ownerVersion + 1,
    reservedSource' = 0,
    reservedDestination' = 0,
    postedSource' = postedSource,
    postedDestination' = postedDestination,
    resolutionCount' = resolutionCount + 1,
    eventCount' = eventCount + 1,
    outboxCount' = outboxCount + 1,
    receiptCount' = receiptCount + 1,
    storedCreateInput' = storedCreateInput,
    storedResolutionInput' = input,
    expiryAt' = expiryAt,
    lastNoOp' = false,
    preserveBefore,
  }

  action expire(expectedVersion: int, input: int, now: int): bool = all {
    phase == Pending,
    expectedVersion == ownerVersion,
    input > 0,
    now >= expiryAt,
    phase' = Expired,
    ownerVersion' = ownerVersion + 1,
    reservedSource' = 0,
    reservedDestination' = 0,
    postedSource' = postedSource,
    postedDestination' = postedDestination,
    resolutionCount' = resolutionCount + 1,
    eventCount' = eventCount + 1,
    outboxCount' = outboxCount + 1,
    receiptCount' = receiptCount + 1,
    storedCreateInput' = storedCreateInput,
    storedResolutionInput' = input,
    expiryAt' = expiryAt,
    lastNoOp' = false,
    preserveBefore,
  }

  action noOp = all {
    phase' = phase,
    ownerVersion' = ownerVersion,
    reservedSource' = reservedSource,
    reservedDestination' = reservedDestination,
    postedSource' = postedSource,
    postedDestination' = postedDestination,
    resolutionCount' = resolutionCount,
    eventCount' = eventCount,
    outboxCount' = outboxCount,
    receiptCount' = receiptCount,
    storedCreateInput' = storedCreateInput,
    storedResolutionInput' = storedResolutionInput,
    expiryAt' = expiryAt,
    lastNoOp' = true,
    beforePhase' = phase,
    beforeOwnerVersion' = ownerVersion,
    beforeReservedSource' = reservedSource,
    beforeReservedDestination' = reservedDestination,
    beforePostedSource' = postedSource,
    beforePostedDestination' = postedDestination,
    beforeResolutionCount' = resolutionCount,
    beforeEventCount' = eventCount,
    beforeOutboxCount' = outboxCount,
    beforeReceiptCount' = receiptCount,
  }

  action exactCreateRetry(input: int): bool = all {
    phase != Empty,
    input == storedCreateInput,
    noOp,
  }

  action exactResolutionRetry(input: int): bool = all {
    phase.in(Set(Posted, Voided, Expired)),
    input == storedResolutionInput,
    noOp,
  }

  action rejectEarlyExpiry(now: int): bool = all {
    phase == Pending,
    now < expiryAt,
    noOp,
  }

  action reject = noOp

  action step = nondet expectedVersion = 0.to(2).oneOf()
    nondet input = 1.to(3).oneOf()
    nondet amount = 1.to(3).oneOf()
    nondet now = 0.to(3).oneOf()
    nondet expires = 1.to(3).oneOf()
    any {
      reserve(expectedVersion, input, amount, expires),
      postAll(expectedVersion, input, amount),
      void(expectedVersion, input),
      expire(expectedVersion, input, now),
      exactCreateRetry(input),
      exactResolutionRetry(input),
      rejectEarlyExpiry(now),
      reject,
    }

  val terminalPhases = Set(Posted, Voided, Expired)

  def typeOk = all {
    phase.in(Set(Empty, Pending, Posted, Voided, Expired)),
    ownerVersion >= 0,
    resolutionCount >= 0,
    eventCount >= 0,
    outboxCount >= 0,
    receiptCount >= 0,
  }

  def conserved = all {
    reservedSource + reservedDestination == 0,
    postedSource + postedDestination == 0,
  }

  def recordsAligned = all {
    ownerVersion == eventCount,
    eventCount == outboxCount,
    outboxCount == receiptCount,
  }

  def terminalResolvedOnce = if (phase.in(terminalPhases))
    resolutionCount == 1
    else resolutionCount == 0

  def noTerminalReservation = if (phase.in(terminalPhases))
    all { reservedSource == 0, reservedDestination == 0 }
    else true

  def pendingHasOneCreate = if (phase == Pending)
    all { ownerVersion == 1, eventCount == 1, storedCreateInput > 0 }
    else true

  def noOpPreservesAuthority = if (lastNoOp)
    all {
      beforePhase == phase,
      beforeOwnerVersion == ownerVersion,
      beforeReservedSource == reservedSource,
      beforeReservedDestination == reservedDestination,
      beforePostedSource == postedSource,
      beforePostedDestination == postedDestination,
      beforeResolutionCount == resolutionCount,
      beforeEventCount == eventCount,
      beforeOutboxCount == outboxCount,
      beforeReceiptCount == receiptCount,
    }
    else true
}
