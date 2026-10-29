module quantity_unit_dimension {
  var certificateUnit: int
  var certificateDimension: int
  var proposedUnit: int
  var proposedDimension: int
  var postingAccepted: bool
  var leftDimension: int
  var rightDimension: int
  var balanceAccepted: bool

  action init = all {
    certificateUnit' = 1,
    certificateDimension' = 1,
    proposedUnit' = 1,
    proposedDimension' = 1,
    postingAccepted' = false,
    leftDimension' = 1,
    rightDimension' = 1,
    balanceAccepted' = false,
  }

  action attemptPosting(unit: int, dimension: int): bool = all {
    certificateUnit' = certificateUnit,
    certificateDimension' = certificateDimension,
    proposedUnit' = unit,
    proposedDimension' = dimension,
    postingAccepted' = (unit == certificateUnit and dimension == certificateDimension),
    leftDimension' = leftDimension,
    rightDimension' = rightDimension,
    balanceAccepted' = balanceAccepted,
  }

  action attemptBalance(left: int, right: int): bool = all {
    certificateUnit' = certificateUnit,
    certificateDimension' = certificateDimension,
    proposedUnit' = proposedUnit,
    proposedDimension' = proposedDimension,
    postingAccepted' = postingAccepted,
    leftDimension' = left,
    rightDimension' = right,
    balanceAccepted' = left == right,
  }

  action noOp: bool = all {
    certificateUnit' = certificateUnit,
    certificateDimension' = certificateDimension,
    proposedUnit' = proposedUnit,
    proposedDimension' = proposedDimension,
    postingAccepted' = postingAccepted,
    leftDimension' = leftDimension,
    rightDimension' = rightDimension,
    balanceAccepted' = balanceAccepted,
  }

  action step = nondet unit = 1.to(2).oneOf()
    nondet dimension = 1.to(2).oneOf()
    nondet otherDimension = 1.to(2).oneOf()
    any {
      attemptPosting(unit, dimension),
      attemptBalance(dimension, otherDimension),
      noOp,
    }

  def typeOk = all {
    certificateUnit >= 1 and certificateUnit <= 2,
    certificateDimension >= 1 and certificateDimension <= 2,
    proposedUnit >= 1 and proposedUnit <= 2,
    proposedDimension >= 1 and proposedDimension <= 2,
    leftDimension >= 1 and leftDimension <= 2,
    rightDimension >= 1 and rightDimension <= 2,
  }

  def certificateRemainsPinned =
    certificateUnit == 1 and certificateDimension == 1

  def acceptedPostingHasExactBinding =
    postingAccepted implies all {
      proposedUnit == certificateUnit,
      proposedDimension == certificateDimension,
    }

  def differentDimensionCannotSubstitute =
    proposedDimension != certificateDimension implies not(postingAccepted)

  def differentDimensionsCannotBalance =
    leftDimension != rightDimension implies not(balanceAccepted)
}
