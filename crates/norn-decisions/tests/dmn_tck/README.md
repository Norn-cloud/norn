# DMN TCK evidence boundary

Source: `dmn-tck/tck@0456e1a2fa5b9c2382cc7380cc6b95f0031e4793`.
Run `translate.py <pinned checkout> <this directory>` to reproduce JSON files.
The translator verifies the checkout commit. No new engine semantics are implemented.

All 51 original upstream cases across 17 translated models remain blocking,
as do the 10 Norn cases. Equivalence evidence covers 13 models over non-null,
declared-domain inputs. The four other models are explicitly excluded from that
claim, with one successful-execution no-hit witness per model:

| Model | Issue | Reason | Witness input | DMN result | Known translated result |
| --- | --- | --- | --- | --- | --- |
| 0108-first-hitpolicy | #2929 | Declared defaults discarded | Age 19, High, affordable | `{Status: "Declined", Rate: "Standard"}` | null / no command |
| 0109-ruleOrder-hitpolicy | #2929 | Declared defaults discarded | Age 19, High, affordable | `{Status: "Declined", Rate: "Standard"}` | `[]` in a command |
| 0110-outputOrder-hitpolicy | #2930 | Empty nonaggregate result differs | Age 19, Medium, unaffordable | null / no command | `[]` in a command |
| 0119-multi-collect-hitpolicy | #2930 | Empty nonaggregate result differs | Age 19, Medium, unaffordable | null / no command | `[]` in a command |

DMN 1.5 section 10.3.2.10 step 2 returns the declared default (a context for
multiple outputs), or null if absent, before hit-policy aggregation. Source:
https://www.omg.org/spec/DMN/1.5/PDF. Defaults in witnesses are derived from XML.

Other models are retained after auditing their pinned XML: 0010 and 0117 cover
all non-null inputs in their declared `RiskCategory` inputValues domain
(High/Low/Medium), numeric Age, and boolean isAffordable, so their declared
defaults are unreachable in that domain. 0112 and 0113 cover every numeric Age,
so their empty-hit mismatch is unreachable there. An outside-domain string or
missing/null input is not used to fabricate a new applicable gap.

The manifest separates `included` models from `known_gaps`, and lists the latter
under upstream exclusions with model, issue, and one-line reason. All original
fixtures still run against their original expected outputs. The separate witness
test requires admission and evaluation to succeed, then requires a mismatch with
DMN's exact expected output and equality with the exact documented current output.
An error, unrelated output, or a formerly excluded witness that now passes fails
CI. When repaired, remove that model from the translator's `KNOWN_GAPS`, regenerate
so it moves to `included` and loses its exclusion and witness, and retain its
upstream cases. Inventory assertions reject duplicate models/cases,
missing/extra fixtures, missing/extra witnesses, and extra exclusions; no tests are
ignored. This evidence does not claim full DMN or FEEL conformance.
