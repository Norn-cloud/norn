module projection_trust {
  var transitions: List[int]
  var projected: int
  var chainIntact: bool
  var opened: bool
  var command: str
  var amount: int
  var outcome: str
  action init = all { transitions' = [], projected' = 0, chainIntact' = true,
    opened' = true, command' = "init", amount' = 0, outcome' = "init" }
  def folded = transitions.foldl(0, (a, x) => a + x)
  action appendTransition(n: int): bool = all {
    opened, transitions.length() < 4, command' = "append", amount' = n,
    transitions' = transitions.append(n), projected' = projected + n,
    chainIntact' = chainIntact, opened' = true, outcome' = "commit",
  }
  action tamper = all { command' = "tamper", amount' = 0,
    projected' = projected + 1, transitions' = transitions,
    chainIntact' = chainIntact, opened' = false, outcome' = "closed" }
  action corruptChain = all { command' = "corrupt_chain", amount' = 0,
    projected' = projected, transitions' = transitions, chainIntact' = false,
    opened' = false, outcome' = "closed" }
  action reopen = all { command' = "reopen", amount' = 0,
    projected' = projected, transitions' = transitions, chainIntact' = chainIntact,
    opened' = (chainIntact and projected == folded),
    outcome' = if (chainIntact and projected == folded) "open" else "refuse" }
  action step = nondet n = 1.to(2).oneOf()
    any { appendTransition(n), tamper, corruptChain, reopen }
  // Deterministic supplement: successful reopen, append, coherent cache forgery,
  // intact-chain refusal, then chain-corrupt refusal. All use the original actions.
  action scenario_step =
    if (command == "init") reopen
    else if (command == "reopen" and opened) appendTransition(1)
    else if (command == "append") tamper
    else if (command == "tamper") reopen
    else if (command == "reopen" and chainIntact) corruptChain
    else reopen
  def verifiedFold = opened implies (chainIntact and projected == folded)
  def mismatchRefuses = if (command == "reopen")
    ((not(chainIntact) or projected != folded) implies outcome == "refuse") else true
  def invariant = verifiedFold and mismatchRefuses
  temporal safety = always(invariant)
}
