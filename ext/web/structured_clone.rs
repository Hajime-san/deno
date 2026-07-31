// Copyright 2018-2026 the Deno authors. MIT license.

use std::any::Any;
use std::borrow::Cow;
use std::collections::HashMap;

use deno_core::OpState;
use deno_core::StructuredCloneHostObjectRegistry;
use deno_core::StructuredDeserializeWithTransferResult;
use deno_core::op2;
use deno_core::read_structured_clone_host_object;
use deno_core::structured_deserialize_with_transfer;
use deno_core::structured_serialize_with_transfer;
use deno_core::v8;
use deno_core::webidl::ContextFn;
use deno_core::webidl::WebIdlConverter;
use deno_core::webidl::WebIdlError;
use deno_core::webidl::WebIdlErrorKind;
use deno_core::write_structured_clone_host_object;
use deno_error::JsErrorBox;

use crate::image_data::ImageData;

// https://source.chromium.org/chromium/chromium/src/+/main:third_party/blink/renderer/bindings/core/v8/serialization/v8_script_value_serializer.cc
// https://source.chromium.org/chromium/chromium/src/+/main:third_party/blink/renderer/bindings/core/v8/serialization/v8_script_value_deserializer.cc
// https://source.chromium.org/chromium/chromium/src/+/main:third_party/blink/renderer/bindings/core/v8/serialization/serialization_tag.h

/// Version of the Deno-controlled envelope and all host-object payloads. It is
/// the backward-compatibility boundary for bytes that outlive the runtime that
/// wrote them. In particular, IndexedDB stores values produced by
/// StructuredSerializeForStorage and a newer Deno may read those values later:
/// https://w3c.github.io/IndexedDB/#value-construct
///
/// New readers must therefore preserve decoding of older payloads. `ImageData` is
/// an example of how to evolve a payload compatibly: its settings are optional
/// subtags terminated by End. If an older payload has no `PredefinedColorSpace` or
/// `PixelFormat` subtag, the reader falls back to the Web API defaults, `srgb` and
/// `rgba-unorm8`. Adding another optional subtag with a compatible default does
/// not require a version bump. This guarantees new-reader/old-data compatibility;
/// it does not require an old reader to understand a new subtag.
///
/// Bump this version when old bytes require a different interpretation, such as:
/// - changing the order, width, encoding, or meaning of existing payload data;
/// - changing the interpretation of an existing host-object tag or subtag;
/// - adding, removing, or changing a required field without a compatible
///   default.
///
/// A new self-contained host-object tag also does not by itself require a bump.
///
/// When bumping the version, each affected host-object reader must branch at
/// the version boundary and retain the old decoding path for stored payloads.
/// Unaffected readers continue using the same path for both versions.
const WEB_STRUCTURED_CLONE_WIRE_FORMAT_VERSION: u32 = 1;

static TRANSFER_STR: deno_core::FastStaticString =
  deno_core::ascii_str!("transfer");

// Host-object tags are written as exactly one raw byte before the type-specific
// payload. Values are permanent wire identifiers: never renumber, reorder by
// implicit discriminant, or reuse a retired value.
// https://source.chromium.org/chromium/chromium/src/+/main:third_party/blink/renderer/bindings/core/v8/serialization/serialization_tag.h
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum StructuredCloneHostObjectTag {
  // settings:(ImageDataSerializationTag, value)*, End, width:uint32,
  // height:uint32, data:V8 value -> ImageData (ref)
  ImageData = b'#',
  ImageBitmap = b'g', // tags terminated by ImageSerializationTag::kEnd (see
  // SerializedColorParams.h), width:uint32_t,
  // height:uint32_t, pixelDataLength:uint32_t,
  // data:byte[pixelDataLength]
  // -> ImageBitmap (ref)
  // transferId:uint32 -> ImageBitmap pre-created from the matching
  // out-of-band transfer data holder.
  ImageBitmapTransfer = b'G',
  // transferId:uint32 -> OffscreenCanvas pre-created from the matching
  // out-of-band transfer data holder.
  OffscreenCanvasTransfer = b'H',
  #[cfg(test)]
  TestTransferable = b'~',
  // Retired tags must remain reserved as `Deprecated...` variants and must
  // never be assigned to another host object.
}

impl StructuredCloneHostObjectTag {
  fn from_tag(tag: u8) -> Option<Self> {
    match tag {
      tag if tag == Self::ImageData as u8 => Some(Self::ImageData),
      tag if tag == Self::ImageBitmapTransfer as u8 => {
        Some(Self::ImageBitmapTransfer)
      }
      tag if tag == Self::OffscreenCanvasTransfer as u8 => {
        Some(Self::OffscreenCanvasTransfer)
      }
      #[cfg(test)]
      tag if tag == Self::TestTransferable as u8 => {
        Some(Self::TestTransferable)
      }
      _ => None,
    }
  }
}

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
struct TransferableHandler {
  tag: StructuredCloneHostObjectTag,
  validate: ValidateTransferHandler,
  transfer: TransferHandler,
}

pub struct WebStructuredCloneTransferData {
  receive: ReceiveTransferHandler,
  data: Box<dyn Any>,
}

#[derive(Clone)]
pub struct WebStructuredCloneHostObjectRegistry {
  // Runtime dispatch is keyed by Web IDL interface identity, not by the Rust
  // type implementing that interface and not by its persistent wire tag.
  // Hashing the fixed interface name keeps lookup independent of the number
  // of registered host-object codecs.
  serializable_by_interface: HashMap<
    &'static std::ffi::CStr,
    (StructuredCloneHostObjectTag, SerializableHandler),
  >,
  serializable_by_tag:
    HashMap<StructuredCloneHostObjectTag, SerializableHandler>,
  transferable_by_interface:
    HashMap<&'static std::ffi::CStr, TransferableHandler>,
  transferable_by_tag:
    HashMap<StructuredCloneHostObjectTag, &'static std::ffi::CStr>,
}

impl WebStructuredCloneHostObjectRegistry {
  fn new() -> Self {
    Self {
      serializable_by_interface: HashMap::new(),
      serializable_by_tag: HashMap::new(),
      transferable_by_interface: HashMap::new(),
      transferable_by_tag: HashMap::new(),
    }
  }

  pub fn register_serializable<T: deno_core::StructuredCloneHostObject>(
    &mut self,
    tag: StructuredCloneHostObjectTag,
  ) {
    assert!(
      !self.transferable_by_tag.contains_key(&tag),
      "structured clone tag registered twice"
    );
    let handler = SerializableHandler {
      write: write_structured_clone_host_object::<T>,
      read: read_structured_clone_host_object::<T>,
    };
    assert!(
      self
        .serializable_by_interface
        .insert(T::INTERFACE_NAME, (tag, handler))
        .is_none(),
      "structured clone interface registered twice"
    );
    assert!(
      self.serializable_by_tag.insert(tag, handler).is_none(),
      "structured clone tag registered twice"
    );
  }

  pub fn register_transferable<T: deno_core::StructuredCloneTransferable>(
    &mut self,
    tag: StructuredCloneHostObjectTag,
  ) {
    assert!(
      !self.serializable_by_tag.contains_key(&tag),
      "structured clone tag registered twice"
    );
    let interface_name = T::INTERFACE_NAME;
    assert!(
      self
        .transferable_by_interface
        .insert(
          interface_name,
          TransferableHandler {
            tag,
            validate: deno_core::validate_structured_clone_transferable::<T>,
            transfer: transfer_host_object::<T>,
          },
        )
        .is_none(),
      "structured clone transferable interface registered twice"
    );
    assert!(
      self
        .transferable_by_tag
        .insert(tag, interface_name)
        .is_none(),
      "structured clone tag registered twice"
    );
  }
}

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
    #[cfg(test)]
    registry.register_transferable::<test_transferable::TestTransferable>(
      StructuredCloneHostObjectTag::TestTransferable,
    );
    registry
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
    WEB_STRUCTURED_CLONE_WIRE_FORMAT_VERSION
  }

  fn is_host_object<'s, 'i>(
    &self,
    scope: &mut v8::PinScope<'s, 'i>,
    object: v8::Local<'s, v8::Object>,
  ) -> bool {
    let Some(interface_name) = host_object_interface_name(scope, object) else {
      return false;
    };
    self.serializable_by_interface.contains_key(interface_name)
      || self.transferable_by_interface.contains_key(interface_name)
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
      let handler = self.transferable_by_interface.get(interface_name)?;
      serializer.write_raw_bytes(&[handler.tag as u8]);
      serializer.write_uint32(transfer_id);
      return Some(true);
    }
    let interface_name = host_object_interface_name(scope, object)?;
    let (tag, handler) = self.serializable_by_interface.get(interface_name)?;
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
    // TODO:
    // needs cheking wheather the interface exposed to the transfer target realm?
    // https://source.chromium.org/chromium/chromium/src/+/main:third_party/blink/renderer/bindings/core/v8/serialization/v8_script_value_deserializer.cc;l=1043?q=envelope%20v8&ss=chromium%2Fchromium%2Fsrc
    if let Some(expected_interface_name) =
      self.transferable_by_tag.get(&tag).copied()
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
    let handler = self.serializable_by_tag.get(&tag)?;
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
    let Some(handler) = self.transferable_by_interface.get(interface_name)
    else {
      return Ok(false);
    };
    (handler.validate)(scope, object)?;
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
      .transferable_by_interface
      .get(interface_name)
      .ok_or_else(|| {
        JsErrorBox::new(
          "DOMExceptionDataCloneError",
          "Host object is not transferable",
        )
      })?;
    (handler.transfer)(scope, object)
  }

  fn receive_host_object<'s, 'i>(
    &self,
    scope: &mut v8::PinScope<'s, 'i>,
    data: Self::TransferData,
  ) -> Result<v8::Local<'s, v8::Object>, JsErrorBox> {
    (data.receive)(scope, data.data)
  }
}

struct StructuredSerializeOptions<'s> {
  transfer: Vec<v8::Local<'s, v8::Value>>,
}

impl<'s> StructuredSerializeOptions<'s> {
  fn convert<'i>(
    scope: &mut v8::PinScope<'s, 'i>,
    value: Option<v8::Local<'s, v8::Value>>,
  ) -> Result<Self, WebIdlError> {
    let Some(value) = value.filter(|value| !value.is_null_or_undefined())
    else {
      return Ok(Self { transfer: vec![] });
    };
    let object = value.try_cast::<v8::Object>().map_err(|_| {
      WebIdlError::new(
        Cow::Borrowed("Failed to execute 'structuredClone'"),
        ContextFn::new_borrowed(&|| Cow::Borrowed("Argument 2")),
        WebIdlErrorKind::ConvertToConverterType("dictionary"),
      )
    })?;
    let key = TRANSFER_STR.v8_string(scope).unwrap();
    let transfer = object
      .get(scope, key.into())
      .unwrap_or_else(|| v8::undefined(scope).into());
    let transfer = if transfer.is_undefined() {
      vec![]
    } else {
      Vec::<v8::Local<v8::Value>>::convert(
        scope,
        transfer,
        Cow::Borrowed("Failed to execute 'structuredClone'"),
        ContextFn::new_borrowed(&|| {
          Cow::Borrowed("'transfer' of 'StructuredSerializeOptions'")
        }),
        &Default::default(),
      )?
    };
    Ok(Self { transfer })
  }
}

// https://html.spec.whatwg.org/multipage/structured-data.html#dom-structuredclone
#[op2]
pub fn structured_clone<'s, 'i>(
  state: &mut OpState,
  scope: &mut v8::PinScope<'s, 'i>,
  value: v8::Local<'s, v8::Value>,
  options: Option<v8::Local<'s, v8::Value>>,
) -> Result<v8::Local<'s, v8::Value>, JsErrorBox> {
  let context = scope.get_current_context();
  // Serialization can invoke user code, so do not keep OpState borrowed while
  // V8 walks the graph.
  let registry = state
    .borrow::<WebStructuredCloneHostObjectRegistry>()
    .clone();
  let options = StructuredSerializeOptions::convert(scope, options)
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
    context,
    value,
    &options.transfer,
    &registry,
  )?;
  let StructuredDeserializeWithTransferResult { deserialized, .. } =
    structured_deserialize_with_transfer(
      scope, serialized, context, &registry,
    )?;

  Ok(deserialized)
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use deno_core::JsRuntime;
  use deno_core::RuntimeOptions;
  use deno_core::is_structured_clone_host_object;
  use deno_core::v8;

  use super::WebStructuredCloneHostObjectRegistry;
  use super::test_transferable::TestTransferable;
  use crate::image_data::ImageData;

  // Structured-clone fixtures are compatibility contracts, not snapshots to
  // update mechanically:
  // - Never edit or delete an existing fixture after its format has shipped.
  // - For an incompatible Deno payload change, bump the Deno wire version, add
  //   a new fixture, add a versioned decoder branch, and retain this decode
  //   test for every older fixture.
  // - If only V8's wire version changes, add a fixture whose name contains the
  //   new V8 version and point the encoder test at it. Keep older fixtures in
  //   decoder tests because persisted values may contain the old V8 payload.
  // A failure of the encoder test therefore requires determining whether the
  // Deno-controlled bytes changed before deciding whether to bump its version.

  const IMAGE_DATA_V1_V8_16: &[u8] = &[
    b'D', b'E', b'N', b'O', // Deno embedder magic
    0x01, // Deno wire format version 1
    0xFF, 0x10, // V8 wire format header, version 16
    0x5C, // V8 host-object tag
    b'#', // Deno ImageData tag
    0x01, 0x01, // PredefinedColorSpace: display-p3
    0x02, 0x00, // PixelFormat: rgba-unorm8
    0x00, // End of ImageData settings
    0x01, 0x01, // width: 1, height: 1
    // V8-serialized Uint8ClampedArray containing the four RGBA bytes.
    0x42, 0x04, 0x01, 0x02, 0x03, 0x04, 0x56, 0x43, 0x00, 0x04, 0x00,
  ];

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
  fn intermediate_serialization_encodes_primitives() {
    let mut runtime = runtime();

    deno_core::scope!(scope, runtime);
    let context = scope.get_current_context();
    let registry = WebStructuredCloneHostObjectRegistry::default();
    let value: v8::Local<v8::Value> = v8::Integer::new(scope, 42).into();
    let bytes = deno_core::structured_serialize_internal(
      scope, context, value, false, &registry,
    )
    .unwrap();

    assert!(bytes.starts_with(b"DENO"));
    let value =
      deno_core::structured_deserialize(scope, bytes, context, &registry)
        .unwrap();
    assert_eq!(value.int32_value(scope), Some(42));
  }

  #[test]
  fn image_data_v1_wire_format() {
    let mut runtime = runtime();
    let value = runtime
      .execute_script(
        "image_data_v1.js",
        r#"
          const { ImageData } = Deno.core.loadExtScript(
            "ext:deno_web/16_image_data.js",
          );
          const value = new ImageData(1, 1, { colorSpace: "display-p3" });
          value.data.set([1, 2, 3, 4]);
          value;
        "#,
      )
      .unwrap();

    deno_core::scope!(scope, runtime);
    let context = scope.get_current_context();
    let registry = WebStructuredCloneHostObjectRegistry::default();
    let value = deno_core::v8::Local::new(scope, value);
    let bytes = deno_core::structured_serialize_internal(
      scope, context, value, false, &registry,
    )
    .unwrap();

    assert_eq!(bytes, IMAGE_DATA_V1_V8_16);
  }

  fn get_property<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
    name: &str,
  ) -> v8::Local<'s, v8::Value> {
    let key = v8::String::new(scope, name).unwrap();
    object.get(scope, key.into()).unwrap()
  }

  #[test]
  fn decodes_image_data_v1_fixture() {
    let mut runtime = runtime();

    deno_core::scope!(scope, runtime);
    let context = scope.get_current_context();
    let registry = WebStructuredCloneHostObjectRegistry::default();
    let value = deno_core::structured_deserialize(
      scope,
      IMAGE_DATA_V1_V8_16.to_vec(),
      context,
      &registry,
    )
    .unwrap();
    let object = value.try_cast::<v8::Object>().unwrap();

    assert!(is_structured_clone_host_object::<ImageData>(scope, object));
    assert_eq!(
      get_property(scope, object, "width")
        .uint32_value(scope)
        .unwrap(),
      1
    );
    assert_eq!(
      get_property(scope, object, "height")
        .uint32_value(scope)
        .unwrap(),
      1
    );
    assert_eq!(
      get_property(scope, object, "colorSpace").to_rust_string_lossy(scope),
      "display-p3"
    );
    assert_eq!(
      get_property(scope, object, "pixelFormat").to_rust_string_lossy(scope),
      "rgba-unorm8"
    );

    let data = get_property(scope, object, "data")
      .try_cast::<v8::Object>()
      .unwrap();
    assert!(data.is_uint8_clamped_array());
    for (index, expected) in [1, 2, 3, 4].into_iter().enumerate() {
      assert_eq!(
        data
          .get_index(scope, index as u32)
          .unwrap()
          .uint32_value(scope)
          .unwrap(),
        expected
      );
    }
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

  #[test]
  fn resolves_host_object_transfer_reference() {
    let mut runtime = runtime();
    deno_core::scope!(scope, runtime);
    let context = scope.get_current_context();
    let registry = WebStructuredCloneHostObjectRegistry::default();
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
      context,
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
