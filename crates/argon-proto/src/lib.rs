// SPDX-License-Identifier: GPL-3.0-or-later
//! Pure, I/O-free codecs for Argon40 Raspberry Pi enclosure and UPS hardware.
//!
//! This crate contains **no I/O of any kind**. It turns bytes into typed values and typed
//! values into bytes, and nothing else. That constraint is what makes it fuzzable,
//! property-testable and runnable under Miri on any machine, with no Raspberry Pi and no
//! Argon hardware present.
//!
//! # Clean-room provenance
//!
//! Every protocol fact implemented here is recorded in `docs/protocol/FACTS.md` with a
//! provenance status. See `CLEANROOM.md` for why. Facts still marked `inferred` are
//! implemented as codecs — a pure function cannot harm a device — but the *write paths*
//! that would put those bytes on a wire are gated separately, in `argon-hal` and
//! `argon-device`. Encoding a frame is safe; transmitting one is a policy decision.
//!
//! # Safety-relevant reading
//!
//! [`ups::Frame`] decoding is an adversarial-input surface: the bytes come from a device we
//! do not control, over a link that can desynchronise. The decoder therefore bounds its
//! resynchronisation buffer, rejects rather than truncates malformed frames, and makes
//! bounded progress per byte. See [`ups::FrameReader`].

#![no_std]
#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod bcd;
pub mod fan;
pub mod ups;
