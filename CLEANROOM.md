# Clean-room development rules

`argon-utils` is licensed **GPL-3.0-or-later**. Argon40's own control software
([`Argon40Tech/Argon40case`](https://github.com/Argon40Tech/Argon40case),
[`Argon40Tech/Argon-ONE-UP`](https://github.com/Argon40Tech/Argon-ONE-UP)) carries
**no LICENSE file at all**, which means it is all-rights-reserved. We therefore may not
copy, translate, or derive our implementation from it.

This is not a formality. On a machine with the official software installed, that
all-rights-reserved Python sits unpacked in **`/etc/argon/`** — roughly 22 files, one `cat`
away at any moment. The rules below exist because the contamination risk is immediate and
ambient, not hypothetical.

## The rules

1. **Do not read upstream implementation source** while contributing implementation code.
   That means the `Argon40case` and `Argon-ONE-UP` repositories, `/etc/argon/*.py`,
   `/etc/argon/*.sh`, and any fork or mirror of them.

2. **Tainted contributors write specifications, not code.** Anyone who *has* read that
   source may contribute to `docs/protocol/` — describing what the hardware does — but must
   not write or review the implementation of the subsystem they read about.

3. **`docs/protocol/` is the only permitted input to implementation.** Every implementation
   pull request cites the fact IDs from [`docs/protocol/FACTS.md`](docs/protocol/FACTS.md)
   that it relies on. If a needed fact is not in the ledger, the fact gets established and
   documented first.

4. **Specifications precede code.** The "we specified it rather than copied it" position is
   only credible if the spec document is older than the implementation. Write the protocol
   doc in the milestone *before* the one that implements it.

## What we are allowed to use

- **Argon40's published protocol documentation** —
  [`Argon40Tech/Argon-ONE-i2c-Codes`](https://github.com/Argon40Tech/Argon-ONE-i2c-Codes) is
  explicitly a public document of the MCU command set. Facts from it are `documented`.
- **Component datasheets** — SSD1306 (Solomon Systech), PCF8563 (NXP), USB HID Power Device
  Class (USB-IF), and so on. Facts from these are `documented`.
- **Observation of the hardware we own** — bus captures, logic-analyser traces, `hidraw`
  descriptors, measured GPIO edge timings. Facts established this way are `observed`, and
  the capture is committed as evidence.

Interoperability reverse-engineering of a device you own is lawful in the EU
(Directive 2009/24/EC Art. 6) and in many other jurisdictions. Reading someone's
unlicensed source code and re-typing it is a different act, and is what these rules exist
to prevent.

## Enforcement

- CI greps the tree for upstream identifier spellings (`argonsysinfo`, `argonregister`,
  `argononed`, …) appearing in our source. A hit fails the build. It is a cheap canary, not
  a proof — the real safeguard is rule 1.
- Every entry in `FACTS.md` carries a provenance status, and `inferred` facts are blocked
  from backing any write path.
