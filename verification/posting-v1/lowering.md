# Receipt/delivery lowering correspondence

`lower_stock_posting` is pure preparation and grants no commit authority.
- POST-VALUE: quantity/value scale 6, GL scale 2; receipt acquisition is
  half-up(q × cost / 10^6). Only `allocate_fifo` chooses delivery allocations.
  Stock GL = round2(sum after) − round2(sum before), never per-line rounding.
- POST-ORDER: manifest-ordered lines and commit-ordered `replay_fifo` history.
- POST-AUTHORITY: master account pins must agree; this is structural evidence.
- POST-ATOMIC: pure refusal preserves inputs, including partial candidates.
- POST-IDENTITY: identical inputs reproduce all movement and leg identities.
CP1-FIFO tests: receive 2 @ 10 + 3 @ 20 (Dr stock 80 / Cr SRBNB 80), deliver
4 (Dr COGS 60 / Cr stock 60), leaving 1 worth 20; replay and balance agree.
Follow-up: checked Post/rule bindings, authorized/fenced reads (including empty
partitions), protected writes, native atomic/retry tests and alpha invoke/replay.
Witnesses cover rounding, zero cost, oversell and invalid inputs; no installed action is claimed.
