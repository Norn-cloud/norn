module closed_action_effect {
  var ownerVersion: int
  var actionVersion: int
  var entityVersion: int
  var actionActive: bool
  var entityActive: bool
  var resultStored: bool
  var storedInput: int
  var effectCount: int
  var eventCount: int
  var committedOwnerVersion: int
  var committedActionVersion: int
  var committedEntityVersion: int
  var committedInputValid: bool
  var committedOutputValid: bool
  var committedOntologyValid: bool

  var lastNoOp: bool
  var beforeOwnerVersion: int
  var beforeResultStored: bool
  var beforeStoredInput: int
  var beforeEffectCount: int
  var beforeEventCount: int

  action init = all {
    ownerVersion' = 0,
    actionVersion' = 1,
    entityVersion' = 1,
    actionActive' = true,
    entityActive' = true,
    resultStored' = false,
    storedInput' = 0,
    effectCount' = 0,
    eventCount' = 0,
    committedOwnerVersion' = 0,
    committedActionVersion' = 0,
    committedEntityVersion' = 0,
    committedInputValid' = false,
    committedOutputValid' = false,
    committedOntologyValid' = false,
    lastNoOp' = false,
    beforeOwnerVersion' = 0,
    beforeResultStored' = false,
    beforeStoredInput' = 0,
    beforeEffectCount' = 0,
    beforeEventCount' = 0,
  }

  action preserveBefore = all {
    beforeOwnerVersion' = beforeOwnerVersion,
    beforeResultStored' = beforeResultStored,
    beforeStoredInput' = beforeStoredInput,
    beforeEffectCount' = beforeEffectCount,
    beforeEventCount' = beforeEventCount,
  }

  action commit(
    expectedOwnerVersion: int,
    pinnedActionVersion: int,
    pinnedEntityVersion: int,
    input: int,
    inputValid: bool,
    outputValid: bool,
    ontologyValid: bool,
  ): bool = all {
    not(resultStored),
    expectedOwnerVersion == ownerVersion,
    actionActive,
    entityActive,
    pinnedActionVersion == actionVersion,
    pinnedEntityVersion == entityVersion,
    input > 0,
    inputValid,
    outputValid,
    ontologyValid,
    ownerVersion' = ownerVersion + 1,
    actionVersion' = actionVersion,
    entityVersion' = entityVersion,
    actionActive' = actionActive,
    entityActive' = entityActive,
    resultStored' = true,
    storedInput' = input,
    effectCount' = effectCount + 1,
    eventCount' = eventCount + 1,
    committedOwnerVersion' = expectedOwnerVersion,
    committedActionVersion' = pinnedActionVersion,
    committedEntityVersion' = pinnedEntityVersion,
    committedInputValid' = inputValid,
    committedOutputValid' = outputValid,
    committedOntologyValid' = ontologyValid,
    lastNoOp' = false,
    preserveBefore,
  }

  action noOpSnapshot = all {
    ownerVersion' = ownerVersion,
    actionVersion' = actionVersion,
    entityVersion' = entityVersion,
    actionActive' = actionActive,
    entityActive' = entityActive,
    resultStored' = resultStored,
    storedInput' = storedInput,
    effectCount' = effectCount,
    eventCount' = eventCount,
    committedOwnerVersion' = committedOwnerVersion,
    committedActionVersion' = committedActionVersion,
    committedEntityVersion' = committedEntityVersion,
    committedInputValid' = committedInputValid,
    committedOutputValid' = committedOutputValid,
    committedOntologyValid' = committedOntologyValid,
    lastNoOp' = true,
    beforeOwnerVersion' = ownerVersion,
    beforeResultStored' = resultStored,
    beforeStoredInput' = storedInput,
    beforeEffectCount' = effectCount,
    beforeEventCount' = eventCount,
  }

  action changeDefinitions = all {
    not(resultStored),
    ownerVersion' = ownerVersion,
    actionVersion' = actionVersion + 1,
    entityVersion' = entityVersion + 1,
    actionActive' = actionActive,
    entityActive' = entityActive,
    resultStored' = resultStored,
    storedInput' = storedInput,
    effectCount' = effectCount,
    eventCount' = eventCount,
    committedOwnerVersion' = committedOwnerVersion,
    committedActionVersion' = committedActionVersion,
    committedEntityVersion' = committedEntityVersion,
    committedInputValid' = committedInputValid,
    committedOutputValid' = committedOutputValid,
    committedOntologyValid' = committedOntologyValid,
    lastNoOp' = false,
    preserveBefore,
  }

  action deactivateAction = all {
    not(resultStored),
    actionActive,
    ownerVersion' = ownerVersion,
    actionVersion' = actionVersion,
    entityVersion' = entityVersion,
    actionActive' = false,
    entityActive' = entityActive,
    resultStored' = resultStored,
    storedInput' = storedInput,
    effectCount' = effectCount,
    eventCount' = eventCount,
    committedOwnerVersion' = committedOwnerVersion,
    committedActionVersion' = committedActionVersion,
    committedEntityVersion' = committedEntityVersion,
    committedInputValid' = committedInputValid,
    committedOutputValid' = committedOutputValid,
    committedOntologyValid' = committedOntologyValid,
    lastNoOp' = false,
    preserveBefore,
  }

  action reject = noOpSnapshot

  action retrySame(input: int): bool = all {
    resultStored,
    input == storedInput,
    noOpSnapshot,
  }

  action retryConflict(input: int): bool = all {
    resultStored,
    input != storedInput,
    noOpSnapshot,
  }

  action step = nondet expectedOwnerVersion = 0.to(2).oneOf()
    nondet pinnedActionVersion = 1.to(3).oneOf()
    nondet pinnedEntityVersion = 1.to(3).oneOf()
    nondet input = 1.to(2).oneOf()
    nondet inputValid = Set(true, false).oneOf()
    nondet outputValid = Set(true, false).oneOf()
    nondet ontologyValid = Set(true, false).oneOf()
    any {
      commit(
        expectedOwnerVersion,
        pinnedActionVersion,
        pinnedEntityVersion,
        input,
        inputValid,
        outputValid,
        ontologyValid,
      ),
      changeDefinitions,
      deactivateAction,
      reject,
      retrySame(input),
      retryConflict(input),
    }

  def typeOk = all {
    ownerVersion >= 0,
    actionVersion >= 1,
    entityVersion >= 1,
    effectCount >= 0,
    eventCount >= 0,
    storedInput >= 0,
  }

  def oneClosedEffectPerCommit = all {
    effectCount == eventCount,
    effectCount == (if (resultStored) 1 else 0),
    ownerVersion == effectCount,
  }

  def acceptedBoundExactDefinitions = if (resultStored)
    all {
      committedActionVersion >= 1,
      committedActionVersion <= actionVersion,
      committedEntityVersion >= 1,
      committedEntityVersion <= entityVersion,
      committedOwnerVersion + 1 == ownerVersion,
    }
    else true

  def acceptedPassedClosedChecks = if (resultStored)
    all {
      committedInputValid,
      committedOutputValid,
      committedOntologyValid,
    }
    else true

  def noOpPreservesAuthority = if (lastNoOp)
    all {
      beforeOwnerVersion == ownerVersion,
      beforeResultStored == resultStored,
      beforeStoredInput == storedInput,
      beforeEffectCount == effectCount,
      beforeEventCount == eventCount,
    }
    else true
}
