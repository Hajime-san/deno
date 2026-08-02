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
    tag: u8,
    handler: SerializableHandler,
  },
  #[allow(dead_code)] // No production Web IDL transferable is registered yet.
  Transferable {
    tag: u8,
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
    tag: u8,
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
    tag: u8,
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
      StructuredCloneHostObjectTag::ImageData as u8,
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
      serializer.write_raw_bytes(&[*tag]);
      serializer.write_uint32(transfer_id);
      return Some(true);
    }
    let interface_name = host_object_interface_name(scope, object)?;
    let HostObjectHandler::Serializable { tag, handler, .. } =
      self.inner.handlers_by_interface.get(interface_name)?
    else {
      return None;
    };
    serializer.write_raw_bytes(&[*tag]);
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

  // Specific primitives have no identity to reconstruct. Keep this optimization at the
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
#[path = "tests/wire_format_backward_compatibility.rs"]
mod wire_format_backward_compatibility;

#[cfg(test)]
mod array_buffer {
  use std::sync::Arc;

  use deno_core::JsRuntime;
  use deno_core::RuntimeOptions;

  fn runtime() -> JsRuntime {
    JsRuntime::new(RuntimeOptions {
      extensions: vec![
        deno_webidl::deno_webidl::init(),
        crate::deno_web::init(
          Arc::new(crate::BlobStore::default())
            as Arc<dyn crate::BlobStoreTrait>,
          None,
          Default::default(),
          Default::default(),
        ),
      ],
      ..Default::default()
    })
  }

  #[test]
  fn transfers_array_buffer() {
    let mut runtime = runtime();
    runtime
        .execute_script(
          "structured_clone_array_buffer_transfer.js",
          r#"
            const { structuredClone } = Deno.core.loadExtScript(
              "ext:deno_web/02_structured_clone.js",
            );
            const fastSource = new ArrayBuffer(4);
            new Uint8Array(fastSource).set([5, 6, 7, 8]);
            const fastClone = structuredClone(fastSource);
            if (fastClone === fastSource || fastSource.byteLength !== 4) {
              throw new Error("ArrayBuffer fast path did not clone");
            }
            if (new Uint8Array(fastClone).join(",") !== "5,6,7,8") {
              throw new Error("ArrayBuffer fast path has invalid data");
            }
            const source = new ArrayBuffer(4);
            const sourceView = new Uint8Array(source);
            sourceView.set([1, 2, 3, 4]);
            const value = { first: source, second: source };
            const cloned = structuredClone(value, { transfer: [source] });
            if (source.byteLength !== 0) throw new Error("source was not detached");
            if (cloned.first !== cloned.second) throw new Error("alias was not preserved");
            if (cloned.first.byteLength !== 4) throw new Error("invalid clone length");
            const bytes = new Uint8Array(cloned.first);
            if (bytes.join(",") !== "1,2,3,4") throw new Error("invalid clone data");
          "#,
        )
        .unwrap();
  }

  #[test]
  fn validates_transfer_list_before_detaching() {
    let mut runtime = runtime();
    runtime
        .execute_script(
          "structured_clone_transfer_validation.js",
          r#"
            const { structuredClone } = Deno.core.loadExtScript(
              "ext:deno_web/02_structured_clone.js",
            );
            const duplicate = new ArrayBuffer(4);
            let duplicateThrew = false;
            try {
              structuredClone(null, { transfer: [duplicate, duplicate] });
            } catch {
              duplicateThrew = true;
            }
            if (!duplicateThrew) throw new Error("duplicate transfer did not throw");
            if (duplicate.byteLength !== 4) {
              throw new Error("duplicate transfer detached its source");
            }

            const serializationFailure = new ArrayBuffer(4);
            let serializationThrew = false;
            try {
              structuredClone(Symbol("not cloneable"), {
                transfer: [serializationFailure],
              });
            } catch {
              serializationThrew = true;
            }
            if (!serializationThrew) throw new Error("serialization failure did not throw");
            if (serializationFailure.byteLength !== 4) {
              throw new Error("failed serialization detached its source");
            }

            const unrelated = new ArrayBuffer(4);
            const primitive = structuredClone(1, { transfer: [unrelated] });
            if (primitive !== 1 || unrelated.byteLength !== 0) {
              throw new Error("unreachable transfer was not processed");
            }
          "#,
        )
        .unwrap();
  }
}

#[cfg(test)]
mod options {
  use std::sync::Arc;

  use deno_core::JsRuntime;
  use deno_core::RuntimeOptions;

  fn runtime() -> JsRuntime {
    JsRuntime::new(RuntimeOptions {
      extensions: vec![
        deno_webidl::deno_webidl::init(),
        crate::deno_web::init(
          Arc::new(crate::BlobStore::default())
            as Arc<dyn crate::BlobStoreTrait>,
          None,
          Default::default(),
          Default::default(),
        ),
      ],
      ..Default::default()
    })
  }

  #[test]
  fn converts_structured_serialize_options_in_rust() {
    let mut runtime = runtime();
    runtime
        .execute_script(
          "structured_clone_options_conversion.js",
          r#"
            const { structuredClone } = Deno.core.loadExtScript(
              "ext:deno_web/02_structured_clone.js",
            );
            let getterCalled = false;
            const options = {
              get transfer() {
                getterCalled = true;
                return [];
              },
            };
            structuredClone(1, options);
            if (!getterCalled) throw new Error("transfer getter was not evaluated");

            try {
              structuredClone(1, 1);
              throw new Error("non-dictionary options did not throw");
            } catch (error) {
              if (!(error instanceof TypeError)) throw error;
            }
          "#,
        )
        .unwrap();
  }
}

#[cfg(test)]
mod serialization {
  use std::sync::Arc;

  use deno_core::JsRuntime;
  use deno_core::RuntimeOptions;

  fn runtime() -> JsRuntime {
    JsRuntime::new(RuntimeOptions {
      extensions: vec![
        deno_webidl::deno_webidl::init(),
        crate::deno_web::init(
          Arc::new(crate::BlobStore::default())
            as Arc<dyn crate::BlobStoreTrait>,
          None,
          Default::default(),
          Default::default(),
        ),
      ],
      ..Default::default()
    })
  }

  use deno_core::v8;

  use super::WebStructuredCloneHostObjectRegistry;

  #[test]
  fn intermediate_serialization_encodes_primitives() {
    let mut runtime = runtime();

    deno_core::scope!(scope, runtime);
    let registry = WebStructuredCloneHostObjectRegistry::default();
    let value: v8::Local<v8::Value> = v8::Integer::new(scope, 42).into();
    let bytes =
      deno_core::structured_serialize_internal(scope, value, false, &registry)
        .unwrap();

    assert!(bytes.starts_with(&[0xFE]));
    let target_realm = scope.get_current_context();
    let value =
      deno_core::structured_deserialize(scope, bytes, target_realm, &registry)
        .unwrap();
    assert_eq!(value.int32_value(scope), Some(42));
  }

  #[test]
  fn serializes_image_data() {
    let mut runtime = runtime();
    runtime
      .execute_script(
        "structured_clone_image_data.js",
        r#"
          const { ImageData } = Deno.core.loadExtScript(
            "ext:deno_web/16_image_data.js",
          );
          const { structuredClone } = Deno.core.loadExtScript(
            "ext:deno_web/02_structured_clone.js",
          );

          const original = new ImageData(2, 1, {
            colorSpace: "display-p3",
          });
          original.data.set([1, 2, 3, 4, 5, 6, 7, 8]);

          const clone = structuredClone(original);

          if (clone === original) throw new Error("ImageData identity was preserved");
          if (clone.constructor !== ImageData) throw new Error("invalid ImageData constructor");
          if (clone.width !== 2 || clone.height !== 1) throw new Error("invalid dimensions");
          if (clone.colorSpace !== "display-p3") throw new Error("invalid color space");
          if (clone.pixelFormat !== "rgba-unorm8") throw new Error("invalid pixel format");
          if (clone.data === original.data) throw new Error("ImageData data identity was preserved");
          if (clone.data.join(",") !== original.data.join(",")) throw new Error("invalid ImageData data");

          clone.data[0] = 255;
          if (original.data[0] !== 1) throw new Error("ImageData data was not copied");
        "#,
      )
      .unwrap();
  }

  #[test]
  fn serializes_float16_image_data() {
    let mut runtime = runtime();
    runtime
      .execute_script(
        "structured_clone_float16_image_data.js",
        r#"
          const { ImageData } = Deno.core.loadExtScript(
            "ext:deno_web/16_image_data.js",
          );
          const { structuredClone } = Deno.core.loadExtScript(
            "ext:deno_web/02_structured_clone.js",
          );

          const original = new ImageData(1, 1, {
            pixelFormat: "rgba-float16",
          });
          original.data.set([1, 2, 3, 4]);

          const clone = structuredClone(original);

          if (clone.data.constructor !== Float16Array) throw new Error("invalid data type");
          if (clone.pixelFormat !== "rgba-float16") throw new Error("invalid pixel format");
          if (clone.colorSpace !== "srgb") throw new Error("invalid color space");
          if (clone.data === original.data) throw new Error("ImageData data identity was preserved");
          if (clone.data.join(",") !== original.data.join(",")) throw new Error("invalid ImageData data");
        "#,
      )
      .unwrap();
  }
}

#[cfg(test)]
mod test_transferable {
  use std::cell::Cell;

  use deno_core::GarbageCollected;
  use deno_core::StructuredCloneTransferable;
  use deno_core::v8;
  use deno_error::JsErrorBox;

  pub(super) struct TestTransferable {
    pub value: u32,
    pub detached: Cell<bool>,
  }

  // SAFETY: TestTransferable contains no references requiring GC tracing.
  unsafe impl GarbageCollected for TestTransferable {
    fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}

    fn get_name(&self) -> &'static std::ffi::CStr {
      <Self as deno_core::WebIdlInterface>::INTERFACE_NAME
    }
  }

  impl StructuredCloneTransferable for TestTransferable {
    type TransferData = u32;

    fn validate_transfer(&self) -> Result<(), JsErrorBox> {
      if self.detached.get() {
        return Err(JsErrorBox::new(
          "DOMExceptionDataCloneError",
          "TestTransferable is detached",
        ));
      }
      Ok(())
    }

    fn transfer<'s, 'i>(
      &self,
      _scope: &mut v8::PinScope<'s, 'i>,
    ) -> Result<Self::TransferData, JsErrorBox> {
      self.detached.set(true);
      Ok(self.value)
    }

    fn receive<'s, 'i>(
      _scope: &mut v8::PinScope<'s, 'i>,
      value: Self::TransferData,
    ) -> Result<Self, JsErrorBox> {
      Ok(Self {
        value,
        detached: Cell::new(false),
      })
    }
  }

  impl deno_core::WebIdlInterface for TestTransferable {
    const INTERFACE_NAME: &'static std::ffi::CStr = c"TestTransferable";
  }

  impl deno_core::WebIdlTransferable for TestTransferable {}
}

#[cfg(test)]
mod host_object_transfer {
  use std::sync::Arc;

  use deno_core::JsRuntime;
  use deno_core::RuntimeOptions;
  use deno_core::v8;

  use super::WebStructuredCloneHostObjectRegistry;
  use super::test_transferable::TestTransferable;

  const TEST_TRANSFERABLE_TAG: u8 = b'~';

  fn runtime() -> JsRuntime {
    JsRuntime::new(RuntimeOptions {
      extensions: vec![
        deno_webidl::deno_webidl::init(),
        crate::deno_web::init(
          Arc::new(crate::BlobStore::default())
            as Arc<dyn crate::BlobStoreTrait>,
          None,
          Default::default(),
          Default::default(),
        ),
      ],
      ..Default::default()
    })
  }

  #[test]
  fn resolves_host_object_transfer_reference() {
    let mut runtime = runtime();
    deno_core::scope!(scope, runtime);
    let context = scope.get_current_context();
    let mut registry = WebStructuredCloneHostObjectRegistry::new();
    registry.register_transferable::<TestTransferable>(TEST_TRANSFERABLE_TAG);
    let first_source = deno_core::cppgc::make_cppgc_object(
      scope,
      TestTransferable {
        value: 42,
        detached: std::cell::Cell::new(false),
      },
    );
    let second_source = deno_core::cppgc::make_cppgc_object(
      scope,
      TestTransferable {
        value: 43,
        detached: std::cell::Cell::new(false),
      },
    );
    let graph = v8::Object::new(scope);
    let first_key = v8::String::new(scope, "first").unwrap();
    let alias_key = v8::String::new(scope, "alias").unwrap();
    let second_key = v8::String::new(scope, "second").unwrap();
    assert_eq!(
      graph.set(scope, first_key.into(), first_source.into()),
      Some(true)
    );
    assert_eq!(
      graph.set(scope, alias_key.into(), first_source.into()),
      Some(true)
    );
    assert_eq!(
      graph.set(scope, second_key.into(), second_source.into()),
      Some(true)
    );

    let result = deno_core::structured_serialize_with_transfer(
      scope,
      graph.into(),
      &[first_source.into(), second_source.into()],
      &registry,
    )
    .unwrap();
    for source in [first_source, second_source] {
      let source_value = deno_core::cppgc::try_unwrap_cppgc_object::<
        TestTransferable,
      >(scope, source.into())
      .unwrap();
      assert!(source_value.detached.get());
    }

    let result = deno_core::structured_deserialize_with_transfer(
      scope, result, context, &registry,
    )
    .unwrap();
    assert_eq!(result.transferred_values.len(), 2);
    let cloned_graph = result.deserialized.try_cast::<v8::Object>().unwrap();
    for (key, transfer_id, expected) in
      [(first_key, 0, 42), (alias_key, 0, 42), (second_key, 1, 43)]
    {
      let cloned = cloned_graph
        .get(scope, key.into())
        .unwrap()
        .try_cast::<v8::Object>()
        .unwrap();
      assert_eq!(cloned, result.transferred_values[transfer_id]);
      let cloned_value = deno_core::cppgc::try_unwrap_cppgc_object::<
        TestTransferable,
      >(scope, cloned.into())
      .unwrap();
      assert_eq!(cloned_value.value, expected);
      assert!(!cloned_value.detached.get());
    }
  }
}
