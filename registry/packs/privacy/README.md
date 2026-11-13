# Privacy pack

The personal-data markings and who may reveal them, for cells that serve
personal data (PRIVACY-1/2, PRIVACY-GUARD-1).

- `privacy.markings.entity`: `pii.contact` projects as a stable pseudonym of a
  `Customer`; `pii.identity` is refused.
- `privacy.reveal.policy`: `admin` and `owner` may reveal either marking, as a
  governed action with a ledger receipt. Every other role sees the projection.

- `overlays/canon.Party.entity.json`: `canon.Party` with its `display_name`
  marked `pii.contact`. A cell that serves personal parties installs it in
  place of the canon artifact, together with the catalog: a marking needs its
  declaration in the same set, so the canon pack itself stays unmarked.

Install it with the definitions a cell is provisioned with: a hosted cell does
not take new definitions after provisioning, and widening who may reveal a
declared marking is refused at install.
