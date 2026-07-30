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

#[derive(Clone, Copy)]
#[repr(u32)]
enum StructuredCloneHostObject {
  ImageData = 1,
}

impl StructuredCloneHostObject {
  const ALL: &[Self] = &[Self::ImageData];

  fn from_tag(tag: u32) -> Option<Self> {
    match tag {
      tag if tag == Self::ImageData as u32 => Some(Self::ImageData),
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
    serializer.write_uint32(host_object as u32);
    host_object.write_payload(scope, object, serializer)
  }

  fn read_host_object<'s, 'i>(
    &self,
    scope: &mut v8::PinScope<'s, 'i>,
    deserializer: &dyn v8::ValueDeserializerHelper,
  ) -> Option<v8::Local<'s, v8::Object>> {
    let mut tag = 0;
    if !deserializer.read_uint32(&mut tag) {
      return None;
    }
    StructuredCloneHostObject::from_tag(tag)?.read_payload(scope, deserializer)
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
