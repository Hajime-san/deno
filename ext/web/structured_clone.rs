// Copyright 2018-2026 the Deno authors. MIT license.

use deno_core::StructuredCloneHostObjectRegistry;
use deno_core::op2;
use deno_core::structured_deserialize;
use deno_core::structured_serialize_internal;
use deno_core::v8;
use deno_error::JsErrorBox;

use crate::image_data::ImageData;

// https://source.chromium.org/chromium/chromium/src/+/main:third_party/blink/renderer/bindings/core/v8/serialization/v8_script_value_serializer.cc
// https://source.chromium.org/chromium/chromium/src/+/main:third_party/blink/renderer/bindings/core/v8/serialization/v8_script_value_deserializer.cc
// https://source.chromium.org/chromium/chromium/src/+/main:third_party/blink/renderer/bindings/core/v8/serialization/serialization_tag.h

// Version of the Deno-controlled envelope and all host-object payloads below.
// This becomes a compatibility boundary when serialized values outlive the
// current runtime. In particular, IndexedDB stores values produced by
// StructuredSerializeForStorage and may read them after Deno is upgraded:
// https://w3c.github.io/IndexedDB/#value-construct
//
// Bump this version when making an incompatible wire-format change, such as:
// - changing the order, width, encoding, or meaning of existing payload data;
// - changing the interpretation of an existing host-object tag or subtag;
// - adding, removing, or changing a required field without a compatible
//   default.
// A new self-contained host-object tag or a backward-compatible optional
// subtag does not by itself require a version bump.
//
// When bumping the version, each affected host-object reader must branch at
// the version boundary and retain its old decoding path for stored payloads.
// Readers for unaffected objects should continue using the same decoding path
// for both the old and new versions.
const WEB_STRUCTURED_CLONE_WIRE_FORMAT_VERSION: u32 = 1;

// Host-object tags are written as exactly one raw byte before the type-specific
// payload. Values are permanent wire identifiers: never renumber, reorder by
// implicit discriminant, or reuse a retired value.
#[derive(Clone, Copy)]
#[repr(u8)]
enum StructuredCloneHostObject {
  // settings:(ImageDataSerializationTag, value)*, End, width:uint32,
  // height:uint32, data:V8 value -> ImageData (ref)
  ImageData = b'#',
  // Retired tags must remain reserved as `Deprecated...` variants and must
  // never be assigned to another host object.
}

impl StructuredCloneHostObject {
  const ALL: &[Self] = &[Self::ImageData];

  fn from_tag(tag: u8) -> Option<Self> {
    match tag {
      tag if tag == Self::ImageData as u8 => Some(Self::ImageData),
      _ => None,
    }
  }

  fn is_host_object<'s, 'i>(
    self,
    scope: &mut v8::PinScope<'s, 'i>,
    object: v8::Local<'s, v8::Object>,
  ) -> bool {
    match self {
      Self::ImageData => {
        ImageData::is_structured_clone_host_object(scope, object)
      }
    }
  }

  fn write_payload<'s, 'i>(
    self,
    scope: &mut v8::PinScope<'s, 'i>,
    object: v8::Local<'s, v8::Object>,
    serializer: &dyn v8::ValueSerializerHelper,
  ) -> Option<bool> {
    match self {
      Self::ImageData => {
        ImageData::write_structured_clone_payload(scope, object, serializer)
      }
    }
  }

  fn read_payload<'s, 'i>(
    self,
    scope: &mut v8::PinScope<'s, 'i>,
    deserializer: &dyn v8::ValueDeserializerHelper,
    _wire_format_version: u32,
  ) -> Option<v8::Local<'s, v8::Object>> {
    match self {
      Self::ImageData => {
        ImageData::read_structured_clone_payload(scope, deserializer)
      }
    }
  }
}

struct WebStructuredCloneHostObjectRegistry;

static HOST_OBJECT_REGISTRY: WebStructuredCloneHostObjectRegistry =
  WebStructuredCloneHostObjectRegistry;

impl StructuredCloneHostObjectRegistry
  for WebStructuredCloneHostObjectRegistry
{
  fn wire_format_version(&self) -> u32 {
    WEB_STRUCTURED_CLONE_WIRE_FORMAT_VERSION
  }

  fn is_host_object<'s, 'i>(
    &self,
    scope: &mut v8::PinScope<'s, 'i>,
    object: v8::Local<'s, v8::Object>,
  ) -> bool {
    StructuredCloneHostObject::ALL
      .iter()
      .copied()
      .any(|host_object| host_object.is_host_object(scope, object))
  }

  fn write_host_object<'s, 'i>(
    &self,
    scope: &mut v8::PinScope<'s, 'i>,
    object: v8::Local<'s, v8::Object>,
    serializer: &dyn v8::ValueSerializerHelper,
  ) -> Option<bool> {
    let host_object = StructuredCloneHostObject::ALL
      .iter()
      .copied()
      .find(|host_object| host_object.is_host_object(scope, object))?;
    serializer.write_raw_bytes(&[host_object as u8]);
    host_object.write_payload(scope, object, serializer)
  }

  fn read_host_object<'s, 'i>(
    &self,
    scope: &mut v8::PinScope<'s, 'i>,
    deserializer: &dyn v8::ValueDeserializerHelper,
    wire_format_version: u32,
  ) -> Option<v8::Local<'s, v8::Object>> {
    let tag = *deserializer.read_raw_bytes(1)?.first()?;
    StructuredCloneHostObject::from_tag(tag)?.read_payload(
      scope,
      deserializer,
      wire_format_version,
    )
  }
}

// https://html.spec.whatwg.org/multipage/structured-data.html#dom-structuredclone
#[op2]
pub fn structured_clone<'s, 'i>(
  scope: &mut v8::PinScope<'s, 'i>,
  value: v8::Local<'s, v8::Value>,
  _options: Option<v8::Local<'s, v8::Object>>,
) -> Result<v8::Local<'s, v8::Value>, JsErrorBox> {
  let context = scope.get_current_context();

  let serialized = structured_serialize_internal(
    scope,
    context,
    value,
    false,
    &HOST_OBJECT_REGISTRY,
  )?;
  let deserialize =
    structured_deserialize(scope, serialized, context, &HOST_OBJECT_REGISTRY)?;

  Ok(deserialize)
}
