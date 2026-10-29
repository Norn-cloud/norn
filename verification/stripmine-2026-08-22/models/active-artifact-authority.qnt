module active_artifact_authority {
  // Finite abstraction: immutable v1 exists initially; two competing plans are
  // prepared from v1 and attempt to publish v2 or v3. activeVersion is the sole
  // execution-authority pointer. Zero means no active version.
  var activeVersion: int
  var hasV1: bool
  var hasV2: bool
  var hasV3: bool
  var planV2PreparedFromV1: bool
  var planV3PreparedFromV1: bool

  action init = all {
    activeVersion' = 1,
    hasV1' = true,
    hasV2' = false,
    hasV3' = false,
    planV2PreparedFromV1' = false,
    planV3PreparedFromV1' = false,
  }

  action prepareV2 = all {
    activeVersion == 1,
    activeVersion' = activeVersion,
    hasV1' = hasV1,
    hasV2' = hasV2,
    hasV3' = hasV3,
    planV2PreparedFromV1' = true,
    planV3PreparedFromV1' = planV3PreparedFromV1,
  }

  action prepareV3 = all {
    activeVersion == 1,
    activeVersion' = activeVersion,
    hasV1' = hasV1,
    hasV2' = hasV2,
    hasV3' = hasV3,
    planV2PreparedFromV1' = planV2PreparedFromV1,
    planV3PreparedFromV1' = true,
  }

  action applyV2 = all {
    planV2PreparedFromV1,
    activeVersion == 1,
    activeVersion' = 2,
    hasV1' = hasV1,
    hasV2' = true,
    hasV3' = hasV3,
    planV2PreparedFromV1' = planV2PreparedFromV1,
    planV3PreparedFromV1' = planV3PreparedFromV1,
  }

  action applyV3 = all {
    planV3PreparedFromV1,
    activeVersion == 1,
    activeVersion' = 3,
    hasV1' = hasV1,
    hasV2' = hasV2,
    hasV3' = true,
    planV2PreparedFromV1' = planV2PreparedFromV1,
    planV3PreparedFromV1' = planV3PreparedFromV1,
  }

  action stutter = all {
    activeVersion' = activeVersion,
    hasV1' = hasV1,
    hasV2' = hasV2,
    hasV3' = hasV3,
    planV2PreparedFromV1' = planV2PreparedFromV1,
    planV3PreparedFromV1' = planV3PreparedFromV1,
  }

  action step = any { prepareV2, prepareV3, applyV2, applyV3, stutter }

  def executable(version: int): bool = all {
    activeVersion == version,
    if (version == 1) hasV1
    else if (version == 2) hasV2
    else if (version == 3) hasV3
    else false,
  }

  def typeOk = all {
    activeVersion >= 1,
    activeVersion <= 3,
  }

  def activePointerReferencesEnvelope = if (activeVersion == 1) hasV1
    else if (activeVersion == 2) hasV2
    else hasV3

  def immutableV1History = hasV1

  def exactlyOneExecutable = all {
    executable(activeVersion),
    not(all { executable(1), executable(2) }),
    not(all { executable(1), executable(3) }),
    not(all { executable(2), executable(3) }),
  }

  // Both plans may have been prepared from v1, but after one wins the guard on
  // the other apply action is false. There is no v2 -> v3 path in this model.
  def competingV1PlansCannotBothPublish = not(all { hasV2, hasV3 })
}
