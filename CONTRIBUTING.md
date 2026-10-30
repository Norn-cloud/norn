# Contributing

Discuss substantial changes in an issue before implementation. Keep patches focused,
include tests for changed behavior, and run `bash scripts/ci/core.sh`.

Contributions must be your original work or carry documented compatible rights.
Do not submit customer data, credentials, vendor schemas, or extracted vendor code.
Generated files must be reproduced from their admitted source and generator.

Norn-owned contributions are published under Apache-2.0. Before a contribution
can merge, CLA Assistant checks agreement to the terms below. Sign only for
yourself or an organisation you are authorised to represent. Never sign for
another contributor. GitHub records your account, agreement version and assent.

## Changing workflows and CI gates

Fork pull requests are data-only: `trusted-policy` rejects any change to
`.github/workflows/` or to the gate files it pins. A `Norn-cloud/norn` branch may
change them once an admin comments `/approve-gates <full 40-character head SHA>`
on the pull request. Posting, editing or deleting a `/approve-gates` comment
re-runs `trusted-policy` for that pull request. Only an unedited comment that
names the exact current head counts.
After any new push, approve again.

Agents post `/approve-gates` from the owner account only after a Sol review has
approved that exact head SHA. Record the Sol verdict on the pull request. Never
post it for a head Sol has not approved, and never edit an earlier comment to
change its SHA. `trusted-policy` checks only the admin comment, not the Sol
review, so this rule binds agents rather than the gate.

## Contributor licence agreement — version 1

These terms cover original code, documentation and other material you intentionally
submit for inclusion in `Norn-cloud/norn` (your "Contributions"). Material clearly
marked as third-party work or as not intended for inclusion is not your original
Contribution. "Maintainers" means the people responsible for that repository.
This agreement is independent of the Apache Software Foundation.

### Individual agreement

1. **Ownership remains yours.** You keep your copyright and may use your work
   elsewhere. This agreement does not assign ownership or require you to provide
   future work or support.
2. **Copyright permission.** You permit the Maintainers, their successors and
   everyone receiving Norn to use, copy, change, publish, distribute and sublicense
   your Contributions. This includes releasing them under Apache-2.0 or another
   licence, including a commercial licence. This non-exclusive permission applies
   everywhere, lasts indefinitely, carries no royalty or fee and cannot be withdrawn.
3. **Patent permission.** For patent claims you control that are necessarily
   practised by your Contribution, alone or combined with Norn as submitted, you
   allow recipients to manufacture it, have a contractor manufacture it, use it,
   market it, sell it, import it and transfer it. This permission has the same
   duration, territory and cost terms as the copyright permission. It ends for a
   recipient who starts patent litigation alleging that your Contribution or Norn
   infringes a patent.
4. **Authority and provenance.** You confirm that you can grant these permissions.
   You identify third-party material and applicable terms instead of claiming it
   as your own. If an employer or another party owns relevant rights, obtain that
   party's permission or have its authorised representative complete the corporate
   agreement before submitting the affected work.
5. **No warranty.** Unless you separately agree otherwise, you provide your
   Contributions without a promise about fitness, correctness or freedom from
   infringement. Tell the Maintainers if you learn that a material statement you
   made about your authority or a Contribution's provenance was incorrect.

### Corporate agreement

An authorised representative may accept this section for an organisation by
identifying its legal name and the contributors it covers in the signing pull
request. The representative confirms that they can bind that organisation.

The organisation grants the copyright and patent permissions in sections 2 and 3
for Contributions it owns that the identified contributors intentionally submit.
It confirms that those submissions are authorised and that disclosed third-party
rights are respected. It retains ownership and makes no additional warranty or
support commitment. It may notify the Maintainers that a contributor is no longer
authorised for future submissions; permissions already granted remain in effect.
Each contributor must also accept the individual agreement for rights they hold.

### Signing

Read these terms, then comment on your pull request:

> I have read the Norn CLA v1 and agree to its applicable individual and corporate terms.

Only sign the corporate terms when authorised, and identify the organisation and
covered contributors as described above. CLA Assistant records assent on the
separate `cla-signatures` branch. A changed agreement uses a new signature version;
an earlier signature must not be presented as acceptance of changed terms.
