// Copyright 2018-2026 the Deno authors. MIT license.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::OnceLock;

use deno_core::StructuredCloneHostObjectRegistry;
use deno_core::StructuredCloneHostObjectTag;
use deno_core::StructuredDeserializeWithTransferResult;
use deno_core::op2;
use deno_core::read_structured_clone_host_object;
use deno_core::structured_deserialize_with_transfer;
use deno_core::structured_serialize_with_transfer;
use deno_core::v8;
use deno_core::write_structured_clone_host_object;
use deno_error::JsErrorBox;

use crate::image_data::ImageData;

type WriteHandler = for<'s, 'i> fn(
  &mut v8::PinScope<'s, 'i>,
  v8::Local<'s, v8::Context>,
  v8::Local<'s, v8::Object>,
  &dyn v8::ValueSerializerHelper,
) -> Option<bool>;
type ReadHandler = for<'s, 'i> fn(
  &mut v8::PinScope<'s, 'i>,
  v8::Local<'s, v8::Context>,
  &dyn v8::ValueDeserializerHelper,
  u32,
) -> Option<v8::Local<'s, v8::Object>>;
type ValidateTransferHandler = for<'s, 'i> fn(
  &mut v8::PinScope<'s, 'i>,
  v8::Local<'s, v8::Object>,
) -> Result<(), JsErrorBox>;
type TransferHandler =
  for<'s, 'i> fn(
    &mut v8::PinScope<'s, 'i>,
    v8::Local<'s, v8::Object>,
  ) -> Result<WebStructuredCloneTransferData, JsErrorBox>;
type ReceiveTransferHandler =
  for<'s, 'i> fn(
    &mut v8::PinScope<'s, 'i>,
    Box<dyn Any>,
  ) -> Result<v8::Local<'s, v8::Object>, JsErrorBox>;

#[derive(Clone, Copy)]
struct SerializableHandler {
  write: WriteHandler,
  read: ReadHandler,
}

#[derive(Clone, Copy)]
enum HostObjectHandler {
  Serializable {
    tag: StructuredCloneHostObjectTag,
    handler: SerializableHandler,
  },
  #[allow(dead_code)] // No production Web IDL transferable is registered yet.
  Transferable {
    tag: StructuredCloneHostObjectTag,
    interface_name: &'static std::ffi::CStr,
    validate: ValidateTransferHandler,
    transfer: TransferHandler,
  },
}

const TAG_COUNT: usize = u8::MAX as usize + 1;

#[derive(Clone)]
struct RegistryInner {
  // Tags are one-byte wire values, so an array avoids hashing and stores each
  // registered handler exactly once for deserialization.
  handlers_by_tag: [Option<HostObjectHandler>; TAG_COUNT],
  // Interface names are the Web IDL identity stored in every CppGC wrapper.
  handlers_by_interface: HashMap<&'static std::ffi::CStr, HostObjectHandler>,
}

struct WebStructuredCloneHostObjectRegistry {
  inner: Arc<RegistryInner>,
}

impl WebStructuredCloneHostObjectRegistry {
  fn new() -> Self {
    Self {
      inner: Arc::new(RegistryInner {
        handlers_by_tag: [None; TAG_COUNT],
        handlers_by_interface: HashMap::new(),
      }),
    }
  }

  fn register_handler<T: deno_core::WebIdlInterface + 'static>(
    &mut self,
    handler: HostObjectHandler,
  ) {
    let inner = Arc::make_mut(&mut self.inner);
    let tag = match handler {
      HostObjectHandler::Serializable { tag, .. }
      | HostObjectHandler::Transferable { tag, .. } => tag,
    };
    assert!(
      inner
        .handlers_by_interface
        .insert(T::INTERFACE_NAME, handler)
        .is_none(),
      "structured clone interface registered twice"
    );
    assert!(
      inner.handlers_by_tag[tag as usize]
        .replace(handler)
        .is_none(),
      "structured clone tag registered twice"
    );
  }

  fn register_serializable<T: deno_core::StructuredCloneHostObject>(
    &mut self,
    tag: StructuredCloneHostObjectTag,
  ) {
    self.register_handler::<T>(HostObjectHandler::Serializable {
      tag,
      handler: SerializableHandler {
        write: write_structured_clone_host_object::<T>,
        read: read_structured_clone_host_object::<T>,
      },
    });
  }

  #[allow(dead_code)] // Used by the local registry in transfer tests.
  fn register_transferable<T: deno_core::StructuredCloneTransferable>(
    &mut self,
    tag: StructuredCloneHostObjectTag,
  ) {
    self.register_handler::<T>(HostObjectHandler::Transferable {
      tag,
      interface_name: T::INTERFACE_NAME,
      validate: deno_core::validate_structured_clone_transferable::<T>,
      transfer: transfer_host_object::<T>,
    });
  }
}

pub struct WebStructuredCloneTransferData {
  receive: ReceiveTransferHandler,
  data: Box<dyn Any>,
}

#[allow(dead_code)] // Referenced by transferable registrations.
fn transfer_host_object<'s, 'i, T: deno_core::StructuredCloneTransferable>(
  scope: &mut v8::PinScope<'s, 'i>,
  object: v8::Local<'s, v8::Object>,
) -> Result<WebStructuredCloneTransferData, JsErrorBox> {
  Ok(WebStructuredCloneTransferData {
    receive: receive_host_object::<T>,
    data: Box::new(deno_core::transfer_structured_clone_host_object::<T>(
      scope, object,
    )?),
  })
}

#[allow(dead_code)] // Referenced by transferable registrations.
fn receive_host_object<'s, 'i, T: deno_core::StructuredCloneTransferable>(
  scope: &mut v8::PinScope<'s, 'i>,
  data: Box<dyn Any>,
) -> Result<v8::Local<'s, v8::Object>, JsErrorBox> {
  let data = data.downcast::<T::TransferData>().map_err(|_| {
    JsErrorBox::new(
      "DOMExceptionDataCloneError",
      "Transfer data has an unexpected type",
    )
  })?;
  deno_core::receive_structured_clone_host_object::<T>(scope, *data)
}

impl Default for WebStructuredCloneHostObjectRegistry {
  fn default() -> Self {
    let mut registry = WebStructuredCloneHostObjectRegistry::new();
    registry.register_serializable::<ImageData>(
      StructuredCloneHostObjectTag::ImageData,
    );
    registry
  }
}

// This registry is process-global and immutable after its first use. It holds
// only the fixed set of host objects supplied by deno_web. Unlike Blink's
// runtime if/else interface checks, the interface hash table and tag array
// provide average O(1) dispatch at the cost of keeping those tables allocated
// for the lifetime of the Deno process.
static WEB_STRUCTURED_CLONE_HOST_OBJECT_REGISTRY: OnceLock<
  WebStructuredCloneHostObjectRegistry,
> = OnceLock::new();

fn web_structured_clone_host_object_registry()
-> &'static WebStructuredCloneHostObjectRegistry {
  WEB_STRUCTURED_CLONE_HOST_OBJECT_REGISTRY
    .get_or_init(WebStructuredCloneHostObjectRegistry::default)
}

fn host_object_interface_name(
  scope: &mut v8::Isolate,
  object: v8::Local<v8::Object>,
) -> Option<&'static std::ffi::CStr> {
  deno_core::cppgc::try_get_cppgc_name(scope, object.into())
}

impl StructuredCloneHostObjectRegistry
  for WebStructuredCloneHostObjectRegistry
{
  type TransferData = WebStructuredCloneTransferData;

  fn wire_format_version(&self) -> u32 {
    deno_core::STRUCTURED_CLONE_WIRE_FORMAT_VERSION
  }

  fn is_host_object<'s, 'i>(
    &self,
    scope: &mut v8::PinScope<'s, 'i>,
    object: v8::Local<'s, v8::Object>,
  ) -> bool {
    let Some(interface_name) = host_object_interface_name(scope, object) else {
      return false;
    };
    self
      .inner
      .handlers_by_interface
      .contains_key(interface_name)
  }

  fn write_host_object<'s, 'i>(
    &self,
    scope: &mut v8::PinScope<'s, 'i>,
    context: v8::Local<'s, v8::Context>,
    object: v8::Local<'s, v8::Object>,
    transfer_id: Option<u32>,
    serializer: &dyn v8::ValueSerializerHelper,
  ) -> Option<bool> {
    if let Some(transfer_id) = transfer_id {
      let interface_name = host_object_interface_name(scope, object)?;
      let HostObjectHandler::Transferable { tag, .. } =
        self.inner.handlers_by_interface.get(interface_name)?
      else {
        return None;
      };
      serializer.write_raw_bytes(&[*tag as u8]);
      serializer.write_uint32(transfer_id);
      return Some(true);
    }
    let interface_name = host_object_interface_name(scope, object)?;
    let HostObjectHandler::Serializable { tag, handler, .. } =
      self.inner.handlers_by_interface.get(interface_name)?
    else {
      return None;
    };
    serializer.write_raw_bytes(&[*tag as u8]);
    (handler.write)(scope, context, object, serializer)
  }

  fn read_host_object<'s, 'i>(
    &self,
    scope: &mut v8::PinScope<'s, 'i>,
    context: v8::Local<'s, v8::Context>,
    deserializer: &dyn v8::ValueDeserializerHelper,
    wire_format_version: u32,
    transferred_host_objects: &[v8::Global<v8::Object>],
  ) -> Option<v8::Local<'s, v8::Object>> {
    let tag = *deserializer.read_raw_bytes(1)?.first()?;
    let tag = StructuredCloneHostObjectTag::from_tag(tag)?;
    // Validate that the transferred object has the interface associated with
    // the serialized tag. TODO: add Blink's additional
    // ExecutionContextExposesInterface-equivalent check once Deno exposes
    // per-realm WebIDL exposure metadata.
    // https://source.chromium.org/chromium/chromium/src/+/main:third_party/blink/renderer/bindings/core/v8/serialization/v8_script_value_deserializer.cc;l=1041-1120?q=ExecutionContextExposesInterface&ss=chromium%2Fchromium%2Fsrc
    if let Some(HostObjectHandler::Transferable {
      interface_name: expected_interface_name,
      ..
    }) = self.inner.handlers_by_tag[tag as usize]
    {
      let mut transfer_id = 0;
      if !deserializer.read_uint32(&mut transfer_id) {
        return None;
      }
      let object = v8::Local::new(
        scope,
        transferred_host_objects.get(transfer_id as usize)?,
      );
      return (host_object_interface_name(scope, object)
        == Some(expected_interface_name))
      .then_some(object);
    }
    let HostObjectHandler::Serializable { handler, .. } =
      self.inner.handlers_by_tag[tag as usize]?
    else {
      return None;
    };
    (handler.read)(scope, context, deserializer, wire_format_version)
  }

  fn validate_transferable_host_object<'s, 'i>(
    &self,
    scope: &mut v8::PinScope<'s, 'i>,
    object: v8::Local<'s, v8::Object>,
  ) -> Result<bool, JsErrorBox> {
    let Some(interface_name) = host_object_interface_name(scope, object) else {
      return Ok(false);
    };
    let Some(HostObjectHandler::Transferable { validate, .. }) =
      self.inner.handlers_by_interface.get(interface_name)
    else {
      return Ok(false);
    };
    (validate)(scope, object)?;
    Ok(true)
  }

  fn transfer_host_object<'s, 'i>(
    &self,
    scope: &mut v8::PinScope<'s, 'i>,
    object: v8::Local<'s, v8::Object>,
  ) -> Result<Self::TransferData, JsErrorBox> {
    let interface_name =
      host_object_interface_name(scope, object).ok_or_else(|| {
        JsErrorBox::new(
          "DOMExceptionDataCloneError",
          "Host object is not transferable",
        )
      })?;
    let handler = self
      .inner
      .handlers_by_interface
      .get(interface_name)
      .ok_or_else(|| {
        JsErrorBox::new(
          "DOMExceptionDataCloneError",
          "Host object is not transferable",
        )
      })?;
    let HostObjectHandler::Transferable { transfer, .. } = handler else {
      return Err(JsErrorBox::new(
        "DOMExceptionDataCloneError",
        "Host object is not transferable",
      ));
    };
    (transfer)(scope, object)
  }

  fn receive_host_object<'s, 'i>(
    &self,
    scope: &mut v8::PinScope<'s, 'i>,
    data: Self::TransferData,
  ) -> Result<v8::Local<'s, v8::Object>, JsErrorBox> {
    (data.receive)(scope, data.data)
  }
}

// https://html.spec.whatwg.org/multipage/structured-data.html#dom-structuredclone
#[op2]
pub fn structured_clone<'s, 'i>(
  scope: &mut v8::PinScope<'s, 'i>,
  value: v8::Local<'s, v8::Value>,
  options: Option<v8::Local<'s, v8::Value>>,
) -> Result<v8::Local<'s, v8::Value>, JsErrorBox> {
  let registry = web_structured_clone_host_object_registry();
  let options = deno_core::StructuredSerializeOptions::convert(scope, options)
    .map_err(JsErrorBox::from_err)?;

  // Primitives have no identity to reconstruct. Keep this optimization at the
  // API boundary: a non-empty transfer list must still be validated and
  // processed even when the cloned value itself is a primitive.
  if options.transfer.is_empty()
    && (value.is_undefined()
      || value.is_null()
      || value.is_boolean()
      || value.is_number()
      || value.is_big_int()
      || value.is_string())
  {
    return Ok(value);
  }

  let serialized = structured_serialize_with_transfer(
    scope,
    value,
    &options.transfer,
    registry,
  )?;
  let StructuredDeserializeWithTransferResult { deserialized, .. } =
    structured_deserialize_with_transfer(
      scope,
      serialized,
      scope.get_current_context(),
      registry,
    )?;

  Ok(deserialized)
}

#[cfg(test)]
#[path = "tests/mod.rs"]
mod tests;
