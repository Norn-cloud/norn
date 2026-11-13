# Privacy pack

The personal-data markings and who may reveal them, for cells that serve
personal data (PRIVACY-1/2, PRIVACY-GUARD-1).

- `privacy.markings.entity`: `pii.contact` projects as a stable pseudonym of a
  `Customer`; `pii.identity` is refused.
- `privacy.reveal.policy`: `admin` and `owner` may reveal either marking, as a
  governed action with a ledger receipt. Every other role sees the projection.

Install it with the definitions a cell is provisioned with: a hosted cell does
not take new definitions after provisioning, and widening who may reveal a
declared marking is refused at install.
