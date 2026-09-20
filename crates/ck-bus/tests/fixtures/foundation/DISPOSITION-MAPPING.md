# Foundation F-PROV disposition mapping

Source: `nats-message-plane-foundation.md` at prefrontal commit
`76a910737c646b72c5c2cc931113ab72a071b38d`, disposition table lines 124-134.

| Foundation row | ck-bus acceptance disposition |
| --- | --- |
| A1 supervised server lifecycle | `A1 supervised server lifecycle` and `A1 argv non-injection` |
| A2 seed absence / child signing | Named exclusion: ck-bus does not receive or scan seed-bearing surfaces; credential signing and launch-principal controls are exercised by install/bootstrap and route-principal rows, while the foundation's audit-read arm remains externally gated. |
| A3 census and revocation | `A3 census write and mint recovery`, `A3 revocation`, `Spawn-stream consumer`, and `Spawn reconciliation and census recovery` |
| A4 stream durability, fanout and re-key | Named exclusion: workload fanout/cursor conformance remains in the prefrontal/commons rigs; CALLO device re-key is not owned by ck-bus. Membership-triggered credential re-mint is covered by `Membership lifecycle`. |
| A5 trust and link | `Leaf configuration` |
| A6 trait conformance | Named exclusion: remains the commons/prefrontal trait conformance rig; no subconscious ladder file owns it. |
| A7 no regression | Named exclusion: remains the prefrontal delivery-path rig and is represented by the external `A6/A7 prefrontal re-run` row. |
| A8 sentinel probe | `Module health answer` and `A8 sentinel probe` |
| A9 grant conformance on a live server | `A9 grant conformance`, `Install bootstrap and self-credential`, `Dead-letter`, and `Membership lifecycle`; prefrontal golden regeneration remains externally gated. |
