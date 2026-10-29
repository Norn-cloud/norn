module quantity_ledger {
  type Status = Open | Pending | Posted | Voided | Expired

  var status: Status
  var reserved: int
  var postedDebit: int
  var postedCredit: int
  var resolutionCount: int

  action init = all {
    status' = Open,
    reserved' = 0,
    postedDebit' = 0,
    postedCredit' = 0,
    resolutionCount' = 0,
  }

  action reserve(amount: int): bool = all {
    status == Open,
    amount > 0,
    status' = Pending,
    reserved' = amount,
    postedDebit' = postedDebit,
    postedCredit' = postedCredit,
    resolutionCount' = resolutionCount,
  }

  action post(amount: int): bool = all {
    status == Pending,
    amount > 0,
    amount == reserved,
    status' = Posted,
    reserved' = 0,
    postedDebit' = postedDebit + amount,
    postedCredit' = postedCredit + amount,
    resolutionCount' = resolutionCount + 1,
  }

  action void = all {
    status == Pending,
    status' = Voided,
    reserved' = 0,
    postedDebit' = postedDebit,
    postedCredit' = postedCredit,
    resolutionCount' = resolutionCount + 1,
  }

  action expire = all {
    status == Pending,
    status' = Expired,
    reserved' = 0,
    postedDebit' = postedDebit,
    postedCredit' = postedCredit,
    resolutionCount' = resolutionCount + 1,
  }

  action stutter = all {
    status' = status,
    reserved' = reserved,
    postedDebit' = postedDebit,
    postedCredit' = postedCredit,
    resolutionCount' = resolutionCount,
  }

  action step = nondet amount = 1.to(3).oneOf()
    any {
      reserve(amount),
      post(amount),
      void,
      expire,
      stutter,
    }

  val statusSet = Set(Open, Pending, Posted, Voided, Expired)

  def typeOk = all {
    status.in(statusSet),
    reserved >= 0,
    postedDebit >= 0,
    postedCredit >= 0,
    resolutionCount >= 0,
  }

  def conserved = postedDebit == postedCredit

  def terminalResolvedOnce =
    if (status == Posted or status == Voided or status == Expired)
    resolutionCount == 1
    else resolutionCount == 0

  def noTerminalReservation =
    if (status == Posted or status == Voided or status == Expired)
    reserved == 0
    else true
}
