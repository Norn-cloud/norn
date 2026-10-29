// CellStore::commit + SqliteCell::commit/check_commit. No UPE/process state.
module commit_admission {
  type Durable = { versions: List[int], values: List[int], inputs: List[int],
    owners: List[int], expected: List[int], commits: int, events: int, effects: int }
  type Request = { owner: int, expected: int, key: int, input: int, seam: int }
  var durable: Durable
  var before: Durable
  var request: Request
  var outcome: str

  pure val initial: Durable = { versions: [0, 0], values: [0, 0], inputs: [0, 0, 0, 0],
    owners: [0, 0, 0, 0], expected: [0, 0, 0, 0], commits: 0, events: 0, effects: 0 }
  action init = all {
    durable' = initial, before' = initial,
    request' = { owner: 0, expected: 0, key: 0, input: 1, seam: 0 },
    outcome' = "init",
  }
  action submit(r: Request): bool = {
    val seen = durable.inputs[r.key] != 0
    val same = durable.inputs[r.key] == r.input and durable.owners[r.key] == r.owner
      and durable.expected[r.key] == r.expected
    val verdict = if (seen) { if (same) "replay" else "conflict" }
      else if (r.expected != durable.versions[r.owner]) "stale"
      else if (r.seam > 0 and r.seam < 6) "rollback"
      else if (r.seam == 6) "lost_ack" else "commit"
    val accepted = verdict == "commit" or verdict == "lost_ack"
    all {
      request' = r, before' = durable, outcome' = verdict,
      durable' = if (accepted) {
        versions: durable.versions.replaceAt(r.owner, durable.versions[r.owner] + 1),
        values: durable.values.replaceAt(r.owner, r.input),
        inputs: durable.inputs.replaceAt(r.key, r.input),
        owners: durable.owners.replaceAt(r.key, r.owner),
        expected: durable.expected.replaceAt(r.key, r.expected),
        commits: durable.commits + 1, events: durable.events + 1, effects: durable.effects + 1
      } else durable,
    }
  }
  action step = {
    nondet owner = 0.to(1).oneOf()
    nondet expected = 0.to(4).oneOf()
    nondet key = 0.to(3).oneOf()
    nondet input = 1.to(2).oneOf()
    nondet seam = 0.to(6).oneOf()
    submit({ owner: owner, expected: expected, key: key, input: input, seam: seam })
  }
  def atomicity = durable.commits == durable.events and durable.events == durable.effects
    and durable.versions.foldl(0, (a, v) => a + v) == durable.commits
  def receiptsOnce = durable.inputs.select(v => v != 0).length() == durable.commits
  def refusalTransparent = if (Set("replay", "conflict", "stale", "rollback").contains(outcome))
    durable == before else true
  def casSafe = if (Set("commit", "lost_ack").contains(outcome))
    request.expected == before.versions[request.owner] else true
  def invariant = atomicity and receiptsOnce and refusalTransparent and casSafe
  temporal safety = always(invariant)
  // Safety only: clients and external consumers have no fairness assumptions.
}
