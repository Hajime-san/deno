// Copyright 2018-2026 the Deno authors. MIT license.

use super::serialize_deserialize::SerializationTag;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TrailerError {
  InvalidHeader,
  InvalidTrailer,
}

#[derive(Default)]
pub(super) struct TrailerWriter {
  // FIXME: Use Vec<SerializationTag> once registry tags are typed and
  // SerializationTag implements a checked TryFrom<u8> for untrusted input.
  required_exposed_interfaces: Vec<u8>,
}

impl TrailerWriter {
  pub(super) fn new() -> Self {
    Self::default()
  }

  pub(super) fn require_exposed_interface(&mut self, tag: u8) {
    // A graph can contain the same interface many times; the trailer only
    // needs one exposure requirement per interface.
    debug_assert_ne!(tag, 0);
    if !self.required_exposed_interfaces.contains(&tag) {
      self.required_exposed_interfaces.push(tag);
    }
  }

  pub(super) fn make_trailer_data(&self) -> Vec<u8> {
    // Graphs containing only values handled by V8 require no host
    // objects, so they have no trailer and retain zero offset/size fields.
    if self.required_exposed_interfaces.is_empty() {
      return Vec::new();
    }

    let interface_count = self.required_exposed_interfaces.len() as u32;
    // A trailer currently contains one record:
    // TrailerRequiresInterfaces:u8 | interface count:u32 big-endian | interface tags:Vec<u8>
    let mut trailer = Vec::with_capacity(
      1 + size_of::<u32>() + self.required_exposed_interfaces.len(),
    );
    trailer.push(SerializationTag::TrailerRequiresInterfaces as u8);
    trailer.extend_from_slice(&interface_count.to_be_bytes());
    trailer.extend_from_slice(&self.required_exposed_interfaces);

    trailer
  }
}

/// It's the size of the sum of the parts of the wire format below
/// | [`EnvelopeHeader::trailer_offset`]:u64 | [`EnvelopeHeader::trailer_size`]:u32
/// See:
/// libs/core/serialization/serialize_deserialize.rs
pub(super) const TRAILER_METADATA_BYTE_LENGTH: usize =
  size_of::<u64>() + size_of::<u32>();
pub(super) const EMPTY_TRAILER_METADATA: [u8; TRAILER_METADATA_BYTE_LENGTH] =
  [0; TRAILER_METADATA_BYTE_LENGTH];

struct EnvelopeHeader {
  wire_format_version: u32,
  /// start index of the V8 header
  payload_start: usize,
  trailer_offset: u64,
  trailer_size: u32,
}

// https://source.chromium.org/chromium/chromium/src/+/main:third_party/blink/renderer/bindings/core/v8/serialization/v8_script_value_deserializer.cc;l=102
fn read_envelope_header(bytes: &[u8]) -> Result<EnvelopeHeader, TrailerError> {
  if bytes.first() != Some(&(SerializationTag::Version as u8)) {
    return Err(TrailerError::InvalidHeader);
  }

  // https://source.chromium.org/chromium/chromium/src/+/main:v8/include/v8-value-serializer.h;l=217-225;
  // > integer types are written in base-128 varint format, not with a binary copy
  // https://en.wikipedia.org/wiki/LEB128
  // u32 needs at most five bytes because each byte contributes
  // a high bit is the continuation flag and seven value bits.
  const VARINT_CONTINUATION_BIT: u8 = 0x80;
  const BASE128_U32_VARINT_MAX_BYTES: usize = 5;

  let mut wire_format_version: u32 = 0;
  // It starts at 1 index due to the SerializationTag::Version itself occupies the first byte above.
  let mut position = 1;
  let mut index = 0;
  loop {
    let byte = *bytes.get(position).ok_or(TrailerError::InvalidHeader)?;
    position += 1;
    // Let's extract payload
    let value = byte & 0x7F;
    // Only four value bits remain in the fifth byte of a u32 varint
    if index == BASE128_U32_VARINT_MAX_BYTES - 1
      && (value > 0x0F // check last four bit
        || // check countinous flag
        byte & VARINT_CONTINUATION_BIT != 0)
    {
      return Err(TrailerError::InvalidHeader);
    }
    // Sum the 7-bit payloads to restore the original u32 value
    wire_format_version |= (value as u32) << (index * 7);
    // check countinous flag
    if byte & VARINT_CONTINUATION_BIT == 0 {
      break;
    }
    index += 1;
  }

  if bytes.get(position) != Some(&(SerializationTag::TrailerOffset as u8)) {
    return Err(TrailerError::InvalidHeader);
  }
  position += 1;

  // The position is at most 7
  // (one-byte version tag + five-byte varint + one-byte trailer tag),
  // so adding the fixed 12-byte metadata length cannot overflow usize.
  let payload_start = position + TRAILER_METADATA_BYTE_LENGTH;
  // If the data is insufficient, the start position of the V8 header
  // cannot be determined, making it impossible for the V8 deserializer
  // to parse it correctly; therefore, it is rejected at this stage.
  let metadata = bytes
    .get(position..payload_start)
    .ok_or(TrailerError::InvalidHeader)?;
  // The range above is exactly 12 bytes, so these 8-byte and 4-byte
  // conversions cannot fail.
  let trailer_offset =
    u64::from_be_bytes(metadata[..size_of::<u64>()].try_into().unwrap());
  let trailer_size =
    u32::from_be_bytes(metadata[size_of::<u64>()..].try_into().unwrap());

  Ok(EnvelopeHeader {
    wire_format_version,
    payload_start,
    trailer_offset,
    trailer_size,
  })
}

pub(super) struct TrailerReader<'a> {
  bytes: &'a [u8],
  wire_format_version: u32,
  v8_payload: &'a [u8],
  trailer: &'a [u8],
  required_exposed_interfaces: Vec<u8>,
  skipped_to_trailer: bool,
  read: bool,
}

impl<'a> TrailerReader<'a> {
  pub(super) fn new(bytes: &'a [u8]) -> Self {
    Self {
      bytes,
      wire_format_version: 0,
      v8_payload: &[],
      trailer: &[],
      required_exposed_interfaces: Vec::new(),
      skipped_to_trailer: false,
      read: false,
    }
  }

  pub(super) fn skip_to_trailer(&mut self) -> Result<bool, TrailerError> {
    // This is a single-pass reader. Re-entering would be an internal API
    // misuse, not malformed serialized data. Keep the state so read() can
    // verify that the header and trailer range were prepared first.
    debug_assert!(!self.skipped_to_trailer);
    self.skipped_to_trailer = true;

    let header = read_envelope_header(self.bytes)?;
    self.wire_format_version = header.wire_format_version;
    // The serializer always reserves both metadata fields, but leaves them
    // zero when the graph contains no host object requiring an exposure check.
    // In that valid new-format case, the V8 payload extends to the buffer end.
    if header.trailer_offset == 0 && header.trailer_size == 0 {
      self.v8_payload = &self.bytes[header.payload_start..];
      return Ok(false);
    }

    let trailer_offset = usize::try_from(header.trailer_offset)
      .map_err(|_| TrailerError::InvalidHeader)?;
    let trailer_size = header.trailer_size as usize;
    let trailer_end = trailer_offset
      .checked_add(trailer_size)
      .ok_or(TrailerError::InvalidHeader)?;
    // The offset is measured from the beginning of the complete serialized
    // buffer. A trailer cannot overlap the envelope or extend past the buffer.
    if trailer_offset < header.payload_start || trailer_end > self.bytes.len() {
      return Err(TrailerError::InvalidHeader);
    }

    self.v8_payload = &self.bytes[header.payload_start..trailer_offset];
    self.trailer = &self.bytes[trailer_offset..trailer_end];

    Ok(true)
  }

  pub(super) fn read(&mut self) -> Result<(), TrailerError> {
    // read() consumes the trailer selected by skip_to_trailer() exactly once;
    // violating that order is an internal API misuse, not invalid wire data.
    debug_assert!(self.skipped_to_trailer);
    debug_assert!(!self.read);
    self.read = true;

    let mut position = 0;
    let mut saw_required_interfaces = false;
    while position < self.trailer.len() {
      let tag = self.trailer[position];
      position += 1;
      // The trailer is a tagged record sequence for future extensibility.
      // Currently only one required-interfaces record is defined and allowed.
      if tag != SerializationTag::TrailerRequiresInterfaces as u8
        || saw_required_interfaces
      {
        return Err(TrailerError::InvalidTrailer);
      }
      saw_required_interfaces = true;

      // position is within the trailer slice, so adding a fixed u32 width
      // cannot overflow usize. A truncated count is rejected by get() below.
      let count_end = position + size_of::<u32>();
      let interface_count = u32::from_be_bytes(
        self
          .trailer
          .get(position..count_end)
          .ok_or(TrailerError::InvalidTrailer)?
          .try_into()
          // get() returns exactly four bytes, so conversion to a u32 array cannot
          // fail. Trailer counts use fixed-width big-endian encoding.
          .unwrap(),
      ) as usize;
      position = count_end;

      // Slice relative to the remaining trailer so an untrusted count cannot
      // overflow an absolute end-position calculation.
      let interfaces = self.trailer[position..]
        .get(..interface_count)
        .ok_or(TrailerError::InvalidTrailer)?;
      self
        .required_exposed_interfaces
        .extend_from_slice(interfaces);
      position += interface_count;
    }

    Ok(())
  }

  pub(super) fn wire_format_version(&self) -> u32 {
    self.wire_format_version
  }

  pub(super) fn v8_payload(&self) -> &'a [u8] {
    self.v8_payload
  }

  pub(super) fn required_exposed_interfaces(&self) -> &[u8] {
    &self.required_exposed_interfaces
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn message(payload: &[u8]) -> Vec<u8> {
    let mut message = vec![
      SerializationTag::Version as u8,
      1,
      SerializationTag::TrailerOffset as u8,
    ];
    message.extend_from_slice(&EMPTY_TRAILER_METADATA);
    message.extend_from_slice(payload);
    message
  }

  fn message_with_trailer(payload: &[u8], trailer: &[u8]) -> Vec<u8> {
    let mut message = message(payload);
    let trailer_offset = message.len() as u64;
    message.extend_from_slice(trailer);
    // Version 1 is a one-byte varint, so the metadata starts after FF 01 FE.
    message[3..11].copy_from_slice(&trailer_offset.to_be_bytes());
    message[11..15].copy_from_slice(&(trailer.len() as u32).to_be_bytes());
    message
  }

  #[test]
  fn writer_returns_no_data_without_required_interfaces() {
    assert!(TrailerWriter::new().make_trailer_data().is_empty());
  }

  #[test]
  fn writer_deduplicates_required_interfaces() {
    let mut writer = TrailerWriter::new();
    writer.require_exposed_interface(b'#');
    writer.require_exposed_interface(b'#');
    writer.require_exposed_interface(b'~');
    assert_eq!(
      writer.make_trailer_data(),
      [
        SerializationTag::TrailerRequiresInterfaces as u8,
        0,
        0,
        0,
        2,
        b'#',
        b'~',
      ]
    );
  }

  #[test]
  fn reader_reads_required_interfaces() {
    let payload = [0xFF, 0x10, 0x30];
    let mut writer = TrailerWriter::new();
    writer.require_exposed_interface(b'#');
    writer.require_exposed_interface(b'~');
    let trailer = writer.make_trailer_data();
    let serialized = message_with_trailer(&payload, &trailer);

    let mut reader = TrailerReader::new(&serialized);
    assert_eq!(reader.skip_to_trailer(), Ok(true));
    reader.read().unwrap();
    assert_eq!(reader.wire_format_version(), 1);
    assert_eq!(reader.v8_payload(), payload);
    assert_eq!(reader.required_exposed_interfaces(), [b'#', b'~']);
  }

  #[test]
  fn reader_accepts_an_empty_trailer() {
    let payload = [0xFF, 0x10, 0x30];
    let serialized = message(&payload);
    let mut reader = TrailerReader::new(&serialized);
    assert_eq!(reader.skip_to_trailer(), Ok(false));
    reader.read().unwrap();
    assert_eq!(reader.v8_payload(), payload);
    assert!(reader.required_exposed_interfaces().is_empty());
  }

  #[test]
  fn reader_rejects_truncated_headers() {
    for serialized in [
      &[][..],
      &[SerializationTag::Version as u8][..],
      &[SerializationTag::Version as u8, 1][..],
      &[
        SerializationTag::Version as u8,
        1,
        SerializationTag::TrailerOffset as u8,
        0,
      ][..],
    ] {
      let mut reader = TrailerReader::new(serialized);
      assert_eq!(reader.skip_to_trailer(), Err(TrailerError::InvalidHeader));
    }
  }

  #[test]
  fn reader_rejects_invalid_trailer_range() {
    let mut invalid_offset = message(&[0xFF, 0x10]);
    invalid_offset[10] = 1;
    invalid_offset[14] = 1;
    let mut reader = TrailerReader::new(&invalid_offset);
    assert_eq!(reader.skip_to_trailer(), Err(TrailerError::InvalidHeader));

    let mut invalid_size = message(&[0xFF, 0x10]);
    let offset = invalid_size.len() as u64;
    invalid_size[3..11].copy_from_slice(&offset.to_be_bytes());
    invalid_size[11..15].copy_from_slice(&1u32.to_be_bytes());
    let mut reader = TrailerReader::new(&invalid_size);
    assert_eq!(reader.skip_to_trailer(), Err(TrailerError::InvalidHeader));
  }

  #[test]
  fn reader_rejects_invalid_trailer_records() {
    let invalid_trailers: &[&[u8]] = &[
      &[0],
      &[SerializationTag::TrailerRequiresInterfaces as u8, 0, 0, 0],
      &[
        SerializationTag::TrailerRequiresInterfaces as u8,
        0,
        0,
        0,
        2,
        b'#',
      ],
    ];
    for trailer in invalid_trailers {
      let serialized = message_with_trailer(&[0xFF, 0x10], trailer);
      let mut reader = TrailerReader::new(&serialized);
      assert_eq!(reader.skip_to_trailer(), Ok(true));
      assert_eq!(reader.read(), Err(TrailerError::InvalidTrailer));
    }
  }
}
