# Complete declared partition reads

FENCE-COMPLETE: a collection is a declared source/link adjacency partition.
Loading captures its complete canonical membership, including link versions,
alongside source/member fact versions. An empty collection carries an empty
membership expectation, not an omitted expectation. All point reads supplied to
the same decision remain fenced, including absent objects. Loading provides no
authorization; callers retain their existing admitted read and commit authority.

FENCE-BOUNDS: the host must provide a complete bounded snapshot or refuse it.
A prefix, unsupported list operation or oversized list is never input to a
successful decision. Canonical fact snapshots and membership expectations are
charged to one existing SliceMeter; no per-partition reset. Target identities
must match the requested partition and be strictly increasing in the canonical
commit model. Duplicate partition declarations cannot observe different sets.

FENCE-PREPARE: `PartitionSlice::prepare` uses existing decision preparation and
adds every captured membership expectation to the same CellCommit as the
RuleIR facts, commands and timers. There is no independent collection commit.
The existing atomic multi-partition commit boundary retains those expectations.
The captured PartitionSlice is the decision input authority for later declared
effect composition; a bare FactSlice carries point reads only.

FENCE-RACES: insertion into an empty or nonempty partition, removal of a member
link or change to a member fact between loading and committing invalidates the
proposal. The refused commit changes no facts, links, ledger, commands or timers.
The correspondence scenarios exercise each race against the actual reference
host, using a domain-neutral capacity partition and ordinary RuleIR outputs.
Authorization enforcement and declaration-to-evaluator integration remain the
existing/following authority bridge; this primitive does not grant either.
