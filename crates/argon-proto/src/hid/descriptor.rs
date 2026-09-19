// SPDX-License-Identifier: GPL-3.0-or-later OR MPL-2.0
//! A HID report descriptor parser, sufficient for reading a Power Device.

use alloc::vec::Vec;
use core::fmt;

/// Which report stream a field belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemKind {
    /// Device-to-host, delivered by reading the device.
    Input,
    /// Host-to-device.
    Output,
    /// Read or written out of band, by control transfer.
    Feature,
}

/// One field within a report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// Report ID this field belongs to.
    pub report_id: u8,
    /// Which report stream.
    pub kind: ItemKind,
    /// Usage page.
    pub usage_page: u16,
    /// Usage within the page.
    pub usage: u16,
    /// Bit offset within the report payload, excluding the leading report-ID byte.
    pub bit_offset: usize,
    /// Width in bits.
    pub bit_size: u8,
    /// Declared minimum.
    pub logical_min: i32,
    /// Declared maximum.
    pub logical_max: i32,
    /// Raw main-item flags.
    pub flags: u32,
}

impl Field {
    /// Whether the field is constant, i.e. report-only and not host-settable.
    #[must_use]
    pub const fn is_constant(&self) -> bool {
        self.flags & 0x01 != 0
    }

    /// Whether the device may change this value without the host writing it.
    #[must_use]
    pub const fn is_volatile(&self) -> bool {
        self.flags & 0x80 != 0
    }

    /// Whether the field carries a signed value, per its declared range.
    #[must_use]
    pub const fn is_signed(&self) -> bool {
        self.logical_min < 0
    }

    /// Extracts this field's value from a report payload.
    ///
    /// `payload` must exclude the leading report-ID byte. Returns `None` if the payload is
    /// too short — a truncated report yields nothing rather than a value assembled from
    /// bits that were never received.
    #[must_use]
    pub fn extract(&self, payload: &[u8]) -> Option<i64> {
        let size = self.bit_size as usize;
        if size == 0 || size > 32 {
            return None;
        }
        let end = self.bit_offset.checked_add(size)?;
        if end > payload.len().checked_mul(8)? {
            return None;
        }

        let mut raw: u64 = 0;
        for i in 0..size {
            let bit = self.bit_offset + i;
            let byte = payload[bit / 8];
            let value = u64::from((byte >> (bit % 8)) & 1);
            raw |= value << i;
        }

        if self.is_signed() && size < 64 {
            let sign_bit = 1u64 << (size - 1);
            if raw & sign_bit != 0 {
                // Sign-extend.
                let extended = raw | !((1u64 << size) - 1);
                return Some(i64::from_ne_bytes(extended.to_ne_bytes()));
            }
        }
        Some(i64::from_ne_bytes(raw.to_ne_bytes()))
    }
}

/// A parsed report descriptor.
#[derive(Debug, Clone, Default)]
pub struct ReportDescriptor {
    fields: Vec<Field>,
}

impl ReportDescriptor {
    /// Every field, in declaration order.
    #[must_use]
    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    /// Finds the first field with the given kind, usage page and usage.
    #[must_use]
    pub fn find(&self, kind: ItemKind, usage_page: u16, usage: u16) -> Option<&Field> {
        self.fields
            .iter()
            .find(|f| f.kind == kind && f.usage_page == usage_page && f.usage == usage)
    }

    /// All distinct report IDs that carry fields of the given kind.
    #[must_use]
    pub fn report_ids(&self, kind: ItemKind) -> Vec<u8> {
        let mut ids: Vec<u8> = self
            .fields
            .iter()
            .filter(|f| f.kind == kind)
            .map(|f| f.report_id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Parses a report descriptor.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError`] if the descriptor is truncated, nests collections or pushes
    /// state beyond the supported depth, or declares more fields than the limit.
    pub fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        /// Items a descriptor may declare. A device is not trusted to be reasonable.
        const MAX_FIELDS: usize = 4096;
        /// Push/pop nesting depth.
        const MAX_PUSH_DEPTH: usize = 16;

        #[derive(Clone, Copy, Default)]
        struct Global {
            usage_page: u16,
            logical_min: i32,
            logical_max: i32,
            report_size: u8,
            report_count: u16,
            report_id: u8,
        }

        let mut global = Global::default();
        let mut stack: Vec<Global> = Vec::new();
        let mut usages: Vec<u16> = Vec::new();
        let mut usage_min: Option<u16> = None;
        // Bit cursor per (report_id, kind).
        let mut cursors: Vec<((u8, ItemKind), usize)> = Vec::new();
        let mut fields: Vec<Field> = Vec::new();

        let mut i = 0usize;
        while i < bytes.len() {
            let prefix = bytes[i];
            let raw_size = (prefix & 0x03) as usize;
            let size = if raw_size == 3 { 4 } else { raw_size };
            let tag = prefix & 0xFC;

            let data_start = i + 1;
            let data_end = data_start.checked_add(size).ok_or(ParseError::Truncated)?;
            if data_end > bytes.len() {
                return Err(ParseError::Truncated);
            }
            let data = &bytes[data_start..data_end];
            let (uval, sval) = item_value(data);

            match tag {
                // ---- Global items ----
                0x04 => global.usage_page = (uval & 0xFFFF) as u16,
                0x14 => global.logical_min = sval,
                0x24 => {
                    // When the minimum is non-negative the maximum is unsigned; otherwise a
                    // one-byte 0xFF maximum would read as -1 rather than 255.
                    global.logical_max = if global.logical_min >= 0 {
                        i32::from_ne_bytes(uval.to_ne_bytes())
                    } else {
                        sval
                    };
                }
                0x74 => global.report_size = (uval & 0xFF) as u8,
                0x94 => global.report_count = (uval & 0xFFFF) as u16,
                0x84 => global.report_id = (uval & 0xFF) as u8,
                0xA4 => {
                    if stack.len() >= MAX_PUSH_DEPTH {
                        return Err(ParseError::TooDeep);
                    }
                    stack.push(global);
                }
                0xB4 => global = stack.pop().unwrap_or(global),

                // ---- Local items ----
                0x08 => usages.push((uval & 0xFFFF) as u16),
                0x18 => usage_min = Some((uval & 0xFFFF) as u16),
                0x28 => {
                    if let Some(min) = usage_min.take() {
                        let max = (uval & 0xFFFF) as u16;
                        for u in min..=max.max(min) {
                            if usages.len() >= MAX_FIELDS {
                                return Err(ParseError::TooManyFields);
                            }
                            usages.push(u);
                        }
                    }
                }

                // ---- Main items ----
                0x80 | 0x90 | 0xB0 => {
                    let kind = match tag {
                        0x80 => ItemKind::Input,
                        0x90 => ItemKind::Output,
                        _ => ItemKind::Feature,
                    };
                    emit_fields(
                        &mut fields,
                        &mut cursors,
                        kind,
                        uval,
                        &usages,
                        (
                            global.report_id,
                            global.usage_page,
                            global.report_size,
                            global.report_count,
                        ),
                        (global.logical_min, global.logical_max),
                        MAX_FIELDS,
                    )?;
                    usages.clear();
                    usage_min = None;
                }
                0xA0 | 0xC0 => {
                    // Collections do not contribute fields; they only scope usages.
                    usages.clear();
                    usage_min = None;
                }
                _ => {
                    usages.clear();
                    usage_min = None;
                }
            }

            i = data_end;
        }

        Ok(Self { fields })
    }
}

/// Decodes an item's data bytes as both an unsigned and a sign-extended signed value.
///
/// HID item data is little-endian and 0, 1, 2 or 4 bytes wide. Which interpretation applies
/// depends on the item: report sizes and counts are unsigned, a logical minimum is signed,
/// and a logical maximum is signed only when its minimum is negative. Both are produced here
/// so the caller chooses per item rather than re-deriving the width.
fn item_value(data: &[u8]) -> (u32, i32) {
    let uval = data
        .iter()
        .rev()
        .fold(0u32, |acc, b| (acc << 8) | u32::from(*b));
    let sval = match data.len() {
        1 => i32::from(i8::from_ne_bytes([data[0]])),
        2 => i32::from(i16::from_le_bytes([data[0], data[1]])),
        4 => i32::from_le_bytes([data[0], data[1], data[2], data[3]]),
        _ => 0,
    };
    (uval, sval)
}

/// Appends the fields declared by one main item, advancing the report's bit cursor.
#[allow(clippy::too_many_arguments)]
fn emit_fields(
    fields: &mut Vec<Field>,
    cursors: &mut Vec<((u8, ItemKind), usize)>,
    kind: ItemKind,
    flags: u32,
    usages: &[u16],
    shape: (u8, u16, u8, u16),
    range: (i32, i32),
    max_fields: usize,
) -> Result<(), ParseError> {
    let (report_id, usage_page, bit_size, report_count) = shape;
    let (logical_min, logical_max) = range;

    let key = (report_id, kind);
    let idx = cursors
        .iter()
        .position(|(k, _)| *k == key)
        .unwrap_or_else(|| {
            cursors.push((key, 0));
            cursors.len() - 1
        });

    for n in 0..report_count as usize {
        let bit_offset = cursors[idx].1;
        cursors[idx].1 += bit_size as usize;

        // A main item may list fewer usages than it has fields; the last usage applies to
        // the remainder. With no usages at all the item is padding and declares no field.
        let Some(usage) = usages.get(n).or_else(|| usages.last()).copied() else {
            continue;
        };
        if fields.len() >= max_fields {
            return Err(ParseError::TooManyFields);
        }
        fields.push(Field {
            report_id,
            kind,
            usage_page,
            usage,
            bit_offset,
            bit_size,
            logical_min,
            logical_max,
            flags,
        });
    }
    Ok(())
}

/// Why a report descriptor could not be parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// An item's data ran past the end of the descriptor.
    Truncated,
    /// Push items nested deeper than supported.
    TooDeep,
    /// The descriptor declared more fields than the limit.
    TooManyFields,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => f.write_str("report descriptor is truncated"),
            Self::TooDeep => f.write_str("report descriptor nests Push items too deeply"),
            Self::TooManyFields => f.write_str("report descriptor declares too many fields"),
        }
    }
}

impl core::error::Error for ParseError {}
