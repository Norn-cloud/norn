# UBL 2.3 Invoice pack

A standard pack: OASIS UBL 2.3 Invoice mapped onto the REA canon (`canon.Invoice` as a `core.Claim`, supplier and customer as `canon.Party`). No UBL-specific Rust exists, and no schema derived from the OASIS originals is committed.

- `source.read.json`: the admission (OASIS licence, release 2.3, per-file SHA-256) and what the reader projects.
- `source.ts`: our hand-written consumer contract, listing only the fields `records.ts` reads (element local names, native types, requiredness). It is pinned to the originals by `registry/standards/oasis-ubl-2-3.source.json`.
- `records.ts`: the mapping, with declared gaps (payable amount awaits the EN 16931 scale policy; currency and due date are optional in UBL; the originating order is not in the document).
- `ubl.pack.ts`: terms and their correspondences to the UBL types.

The originals are OASIS-copyrighted and recorded as reference-only, so they are not committed. To check the contract against them:

```sh
mkdir -p /tmp/ubl && cd /tmp/ubl && curl -fsSLO https://docs.oasis-open.org/ubl/os-UBL-2.3/UBL-2.3.zip && unzip -q UBL-2.3.zip
cargo run -p norn-formats --bin norn-format-read -- registry/packs/ubl/source.read.json --originals /tmp/ubl
```

Line-level mapping (`InvoiceLine` as a nested map) and the EN 16931 business rules are the next packs; both are pack work only.
