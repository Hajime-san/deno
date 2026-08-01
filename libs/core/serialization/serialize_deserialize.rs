// Copyright 2018-2026 the Deno authors. MIT license.

use std::borrow::Cow;

use deno_core::v8;
use deno_core::v8::ValueDeserializerHelper;
use deno_core::v8::ValueSerializerHelper;
use deno_error::JsErrorBox;

use crate::cppgc::GarbageCollected;
use crate::webidl::ContextFn;
use crate::webidl::WebIdlConverter;
use crate::webidl::WebIdlError;
use crate::webidl::WebIdlErrorKind;

// The structuredClone implementation does not correspond one-to-one with the
// steps in the WHATWG spec.
//
// Recursive serialization and deserialization of ECMAScript built-in object
// graphs is delegated to V8's ValueSerializer and ValueDeserializer. This
// preserves cycles, shared references, and V8's representation of built-ins
// such as Array, Map, Set, Date, and more.
//
// Deno implements platform objects that V8 does not know about. This includes
// Web platform objects such as Blob, whose primary interface is [Serializable],
// and OffscreenCanvas, whose primary interface is [Transferable]. Delegates
// implementing v8::ValueSerializerImpl and v8::ValueDeserializerImpl detect,
// serialize, and deserialize these platform objects within an object graph.
//
// Transfer list validation, transferability checks, ownership transfer, and
// detachment belong to the outer implementation of WHATWG
// StructuredSerializeWithTransfer. The V8 serializer receives the transfer
// state prepared by that layer.

pub enum StructuredCloneTransferData<T> {
  ArrayBuffer(v8::SharedRef<v8::BackingStore>),
  HostObject(T),
}

pub struct StructuredSerializeWithTransferResult<T> {
  pub serialized: Vec<u8>,
  pub transfer_data_holders: Vec<StructuredCloneTransferData<T>>,
}

pub struct StructuredDeserializeWithTransferResult<'s> {
  pub deserialized: v8::Local<'s, v8::Value>,
  pub transferred_values: Vec<v8::Local<'s, v8::Value>>,
}

// https://source.chromium.org/chromium/chromium/src/+/main:third_party/blink/renderer/bindings/core/v8/serialization/v8_script_value_serializer.cc
// https://source.chromium.org/chromium/chromium/src/+/main:third_party/blink/renderer/bindings/core/v8/serialization/v8_script_value_deserializer.cc
// https://source.chromium.org/chromium/chromium/src/+/main:third_party/blink/renderer/bindings/core/v8/serialization/serialization_tag.h

// Deno wraps V8's serialized data in an embedder-controlled version envelope:
//
//   "DENO" | embedder version:uint32(varint) | V8 header | V8 payload
//
// The outer version covers Deno host-object tags and payloads, while the inner
// V8 header carries V8's independently versioned wire format. The envelope is
// consumed before constructing the V8 deserializer. The multi-byte magic does
// not collide with V8's 0xFF version header, so an unversioned legacy payload
// can be distinguished if compatibility is needed later. Increment the
// registry's version whenever an existing Deno payload changes incompatibly.
const EMBEDDER_MAGIC: &[u8; 4] = b"DENO";

/// Version of the Deno-controlled structured-clone envelope and host-object
/// payloads. Persisted structured-clone data must remain readable by newer
/// runtimes, so this is owned by the serialization layer rather than a Web API
/// implementation.
pub const STRUCTURED_CLONE_WIRE_FORMAT_VERSION: u32 = 1;

// Host-object tags are written as exactly one raw byte before the type-specific
// payload. Values are permanent wire identifiers: never renumber, reorder by
// implicit discriminant, or reuse a retired value.
// https://source.chromium.org/chromium/chromium/src/+/main:third_party/blink/renderer/bindings/core/v8/serialization/serialization_tag.h
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum StructuredCloneHostObjectTag {
  // settings:(ImageDataSerializationTag, value)*, End, width:uint32,
  // height:uint32, data:V8 value -> ImageData
  ImageData = b'#',
  // Test-only host object used by structured-clone tests. Keep its wire value
  // reserved even though the implementation is not part of the Web API.
  TestTransferable = b'~',
}

impl StructuredCloneHostObjectTag {
  pub fn from_tag(tag: u8) -> Option<Self> {
    match tag {
      tag if tag == Self::ImageData as u8 => Some(Self::ImageData),
      tag if tag == Self::TestTransferable as u8 => {
        Some(Self::TestTransferable)
      }
      _ => None,
    }
  }
}

// V8's WriteUint32 uses a base-128 varint: each byte contributes seven value
// bits and the high bit indicates that another byte follows. Four bytes carry
// 28 bits, so a u32 may use a fifth byte, whose value is limited to four bits.
const VARINT_VALUE_MASK: u8 = 0x7F;
const VARINT_CONTINUATION_BIT: u8 = 0x80;
const VARINT_VALUE_BITS_PER_BYTE: usize = 7;
const U32_VARINT_MAX_BYTES: usize = 5;
const U32_VARINT_LAST_BYTE_MAX: u8 = 0x0F;

/// Runtime identity of a Web IDL interface.
///
/// This is generated from the IDL interface name rather than from the concrete
/// Rust type implementing it. The name is used only for process-local wrapper
/// dispatch; stable structured-clone tags remain a separate wire concern.
/// Its `GarbageCollected::get_name()` implementation must return this same
/// name so the common CppGC wrapper header exposes the IDL identity.
pub trait WebIdlInterface {
  const INTERFACE_NAME: &'static std::ffi::CStr;
}

/// Marker generated by `#[webidl(serializable)]` for Web IDL interfaces that
/// declare the `[Serializable]` extended attribute.
pub trait WebIdlSerializable: WebIdlInterface {}

/// Marker generated by `#[webidl(transferable)]` for Web IDL interfaces that
/// declare the `[Transferable]` extended attribute.
///
/// This marker records the IDL contract only. The interface must separately
/// implement `StructuredCloneTransferable`, because validation, detachment,
/// transfer data, and receiving steps are specific to each platform object.
pub trait WebIdlTransferable: WebIdlInterface {}

/// Serialization and deserialization steps for a Web IDL `[Serializable]`
/// platform object.
///
/// The wire tag is deliberately not part of this trait. Tags are maintained in
/// one embedder-owned enum so duplicate discriminants are rejected at compile
/// time and retired values remain reserved.
pub trait StructuredCloneHostObject:
  WebIdlSerializable + GarbageCollected + Sized + 'static
{
  fn write_structured_clone_payload<'s, 'i>(
    &self,
    scope: &mut v8::PinScope<'s, 'i>,
    context: v8::Local<'s, v8::Context>,
    serializer: &dyn v8::ValueSerializerHelper,
  ) -> Option<bool>;

  /// If the wire format version changes, processing branching may occur for each object.
  /// https://source.chromium.org/chromium/chromium/src/+/main:third_party/blink/renderer/bindings/core/v8/serialization/v8_script_value_deserializer.cc;l=472-485
  fn read_structured_clone_payload<'s, 'i>(
    scope: &mut v8::PinScope<'s, 'i>,
    context: v8::Local<'s, v8::Context>,
    deserializer: &dyn v8::ValueDeserializerHelper,
    wire_format_version: u32,
  ) -> Option<Self>;
}

/// Transfer and transfer-receiving steps for a Web IDL `[Transferable]`
/// platform object. Transfer data is out-of-band and is never persisted in the
/// structured-clone wire payload.
pub trait StructuredCloneTransferable:
  WebIdlTransferable + GarbageCollected + Sized + 'static
{
  type TransferData: 'static;

  fn validate_transfer(&self) -> Result<(), JsErrorBox>;

  fn transfer<'s, 'i>(
    &self,
    scope: &mut v8::PinScope<'s, 'i>,
  ) -> Result<Self::TransferData, JsErrorBox>;

  fn receive<'s, 'i>(
    scope: &mut v8::PinScope<'s, 'i>,
    data: Self::TransferData,
  ) -> Result<Self, JsErrorBox>;
}

pub fn is_structured_clone_host_object<'s, 'i, T: StructuredCloneHostObject>(
  scope: &mut v8::PinScope<'s, 'i>,
  object: v8::Local<'s, v8::Object>,
) -> bool {
  crate::cppgc::try_unwrap_cppgc_object::<T>(scope, object.into()).is_some()
}

pub fn write_structured_clone_host_object<
  's,
  'i,
  T: StructuredCloneHostObject,
>(
  scope: &mut v8::PinScope<'s, 'i>,
  context: v8::Local<'s, v8::Context>,
  object: v8::Local<'s, v8::Object>,
  serializer: &dyn v8::ValueSerializerHelper,
) -> Option<bool> {
  let value = crate::cppgc::try_unwrap_cppgc_object::<T>(scope, object.into())?;
  // SAFETY: `object` remains live for this V8 serializer callback.
  let value = unsafe { value.as_ref() };
  value.write_structured_clone_payload(scope, context, serializer)
}

pub fn read_structured_clone_host_object<
  's,
  'i,
  T: StructuredCloneHostObject,
>(
  scope: &mut v8::PinScope<'s, 'i>,
  context: v8::Local<'s, v8::Context>,
  deserializer: &dyn v8::ValueDeserializerHelper,
  wire_format_version: u32,
) -> Option<v8::Local<'s, v8::Object>> {
  let value = T::read_structured_clone_payload(
    scope,
    context,
    deserializer,
    wire_format_version,
  )?;
  Some(crate::cppgc::make_cppgc_object(scope, value))
}

pub fn validate_structured_clone_transferable<
  's,
  'i,
  T: StructuredCloneTransferable,
>(
  scope: &mut v8::PinScope<'s, 'i>,
  object: v8::Local<'s, v8::Object>,
) -> Result<(), JsErrorBox> {
  let value = crate::cppgc::try_unwrap_cppgc_object::<T>(scope, object.into())
    .ok_or_else(|| data_clone_error("Transferable has an invalid brand"))?;
  value.validate_transfer()
}

pub fn transfer_structured_clone_host_object<
  's,
  'i,
  T: StructuredCloneTransferable,
>(
  scope: &mut v8::PinScope<'s, 'i>,
  object: v8::Local<'s, v8::Object>,
) -> Result<T::TransferData, JsErrorBox> {
  let value = crate::cppgc::try_unwrap_cppgc_object::<T>(scope, object.into())
    .ok_or_else(|| data_clone_error("Transferable has an invalid brand"))?;
  // SAFETY: `object` remains live for this V8 serializer callback.
  let value = unsafe { value.as_ref() };
  value.transfer(scope)
}

pub fn receive_structured_clone_host_object<
  's,
  'i,
  T: StructuredCloneTransferable,
>(
  scope: &mut v8::PinScope<'s, 'i>,
  data: T::TransferData,
) -> Result<v8::Local<'s, v8::Object>, JsErrorBox> {
  let value = T::receive(scope, data)?;
  Ok(crate::cppgc::make_cppgc_object(scope, value))
}

/// Hooks for platform objects whose serialization is defined by the embedder.
///
/// The hooks are called by V8 while it walks a single object graph, so they
/// must write and read exactly one host-object record per invocation.
pub trait StructuredCloneHostObjectRegistry {
  type TransferData;

  /// Version of the embedder-controlled wire format wrapped around V8 data.
  fn wire_format_version(&self) -> u32;

  fn is_host_object<'s, 'i>(
    &self,
    scope: &mut v8::PinScope<'s, 'i>,
    object: v8::Local<'s, v8::Object>,
  ) -> bool;

  fn write_host_object<'s, 'i>(
    &self,
    scope: &mut v8::PinScope<'s, 'i>,
    context: v8::Local<'s, v8::Context>,
    object: v8::Local<'s, v8::Object>,
    transfer_id: Option<u32>,
    serializer: &dyn v8::ValueSerializerHelper,
  ) -> Option<bool>;

  fn read_host_object<'s, 'i>(
    &self,
    scope: &mut v8::PinScope<'s, 'i>,
    context: v8::Local<'s, v8::Context>,
    deserializer: &dyn v8::ValueDeserializerHelper,
    wire_format_version: u32,
    transferred_host_objects: &[v8::Global<v8::Object>],
  ) -> Option<v8::Local<'s, v8::Object>>;

  fn validate_transferable_host_object<'s, 'i>(
    &self,
    scope: &mut v8::PinScope<'s, 'i>,
    object: v8::Local<'s, v8::Object>,
  ) -> Result<bool, JsErrorBox>;

  fn transfer_host_object<'s, 'i>(
    &self,
    scope: &mut v8::PinScope<'s, 'i>,
    object: v8::Local<'s, v8::Object>,
  ) -> Result<Self::TransferData, JsErrorBox>;

  fn receive_host_object<'s, 'i>(
    &self,
    scope: &mut v8::PinScope<'s, 'i>,
    data: Self::TransferData,
  ) -> Result<v8::Local<'s, v8::Object>, JsErrorBox>;
}

// V8 owns reference tracking while it serializes a complete object graph.
// Keep Web(Deno) platform specific state here as support is added.
struct V8SerializerDelegate<'a, R> {
  _for_storage: bool,
  host_objects: &'a R,
  // Nested host-object values must use the context selected by the caller,
  // not whichever context happens to be current during a V8 callback.
  context: v8::Global<v8::Context>,
  // V8 Map provides identity-based lookup that remains valid if V8 moves an
  // object. Do not key a Rust HashMap by Object::get_identity_hash alone: V8
  // explicitly does not guarantee that those hashes are unique.
  transferred_host_object_ids: v8::Global<v8::Map>,
}

impl<R> v8::ValueSerializerImpl for V8SerializerDelegate<'_, R>
where
  R: StructuredCloneHostObjectRegistry,
{
  fn throw_data_clone_error<'s>(
    &self,
    scope: &mut v8::PinScope<'s, '_>,
    message: v8::Local<'s, v8::String>,
  ) {
    let error = v8::Exception::error(scope, message);
    scope.throw_exception(error);
  }

  fn has_custom_host_object(&self, _isolate: &v8::Isolate) -> bool {
    true
  }

  fn is_host_object<'s, 'i>(
    &self,
    scope: &mut v8::PinScope<'s, 'i>,
    object: v8::Local<'s, v8::Object>,
  ) -> Option<bool> {
    Some(self.host_objects.is_host_object(scope, object))
  }

  fn write_host_object<'s, 'i>(
    &self,
    scope: &mut v8::PinScope<'s, 'i>,
    object: v8::Local<'s, v8::Object>,
    serializer: &dyn v8::ValueSerializerHelper,
  ) -> Option<bool> {
    let context = v8::Local::new(scope, &self.context);
    let transferred_host_object_ids =
      v8::Local::new(scope, &self.transferred_host_object_ids);
    let transfer_id = transferred_host_object_ids
      .get(scope, object.into())
      .and_then(|value| value.try_cast::<v8::Uint32>().ok())
      .map(|value| value.value());
    self.host_objects.write_host_object(
      scope,
      context,
      object,
      transfer_id,
      serializer,
    )
  }
}

struct V8DeserializerDelegate<'a, R> {
  host_objects: &'a R,
  // This is the explicit target realm for nested host-object values.
  target_realm: v8::Global<v8::Context>,
  wire_format_version: u32,
  transferred_host_objects: Vec<v8::Global<v8::Object>>,
}

impl<R> v8::ValueDeserializerImpl for V8DeserializerDelegate<'_, R>
where
  R: StructuredCloneHostObjectRegistry,
{
  fn read_host_object<'s, 'i>(
    &self,
    scope: &mut v8::PinScope<'s, 'i>,
    deserializer: &dyn v8::ValueDeserializerHelper,
  ) -> Option<v8::Local<'s, v8::Object>> {
    let target_realm = v8::Local::new(scope, &self.target_realm);
    self.host_objects.read_host_object(
      scope,
      target_realm,
      deserializer,
      self.wire_format_version,
      &self.transferred_host_objects,
    )
  }
}

// https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal
pub fn structured_serialize_internal<'s, 'i, R>(
  scope: &mut v8::PinScope<'s, 'i>,
  context: v8::Local<'s, v8::Context>,
  value: v8::Local<'s, v8::Value>,
  for_storage: bool,
  host_objects: &R,
) -> Result<Vec<u8>, JsErrorBox>
where
  R: StructuredCloneHostObjectRegistry,
{
  structured_serialize_internal_with_transfers(
    scope,
    context,
    value,
    for_storage,
    host_objects,
    &[],
    &[],
  )
}

fn structured_serialize_internal_with_transfers<'s, 'i, R>(
  scope: &mut v8::PinScope<'s, 'i>,
  context: v8::Local<'s, v8::Context>,
  value: v8::Local<'s, v8::Value>,
  for_storage: bool,
  host_objects: &R,
  transferred_array_buffers: &[(u32, v8::Local<'s, v8::ArrayBuffer>)],
  transferred_host_objects: &[v8::Local<'s, v8::Object>],
) -> Result<Vec<u8>, JsErrorBox>
where
  R: StructuredCloneHostObjectRegistry,
{
  if value.is_symbol() {
    return Err(JsErrorBox::new("DataCloneError", "Cannot serialize Symbol"));
  }

  // V8 owns the recursive object graph traversal, including reference tracking
  // for aliases and cycles. Always produce owned bytes at this intermediate
  // layer so the result can be persisted or moved to another isolate. Callers
  // such as structuredClone may optimize primitives before reaching here.
  serialize_v8_graph(
    scope,
    context,
    value,
    for_storage,
    host_objects,
    transferred_array_buffers,
    transferred_host_objects,
  )
}

enum PreparedTransfer<'s> {
  ArrayBuffer(v8::Local<'s, v8::ArrayBuffer>),
  HostObject(v8::Local<'s, v8::Object>),
}

fn data_clone_error(message: impl Into<String>) -> JsErrorBox {
  JsErrorBox::new("DOMExceptionDataCloneError", message.into())
}

// https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializewithtransfer
pub fn structured_serialize_with_transfer<'s, 'i, R>(
  scope: &mut v8::PinScope<'s, 'i>,
  context: v8::Local<'s, v8::Context>,
  value: v8::Local<'s, v8::Value>,
  transfer_list: &[v8::Local<'s, v8::Value>],
  host_objects: &R,
) -> Result<StructuredSerializeWithTransferResult<R::TransferData>, JsErrorBox>
where
  R: StructuredCloneHostObjectRegistry,
{
  let mut prepared = Vec::with_capacity(transfer_list.len());
  let mut transferred_array_buffers = Vec::new();
  let mut transferred_host_objects = Vec::new();
  let seen = v8::Set::new(scope);

  for transferable in transfer_list.iter().copied() {
    if seen.has(scope, transferable).unwrap_or(false) {
      return Err(data_clone_error("Transfer list contains duplicate object"));
    }
    if seen.add(scope, transferable).is_none() {
      return Err(data_clone_error("Cannot index transfer list"));
    }

    if let Ok(array_buffer) = transferable.try_cast::<v8::ArrayBuffer>() {
      if array_buffer.was_detached() {
        return Err(data_clone_error(
          "Transfer list contains a detached ArrayBuffer",
        ));
      }
      let transfer_id = transferred_array_buffers.len() as u32;
      transferred_array_buffers.push((transfer_id, array_buffer));
      prepared.push(PreparedTransfer::ArrayBuffer(array_buffer));
      continue;
    }

    let Ok(object) = transferable.try_cast::<v8::Object>() else {
      return Err(data_clone_error(
        "Value in transfer list is not transferable",
      ));
    };
    if !host_objects.validate_transferable_host_object(scope, object)? {
      return Err(data_clone_error(
        "Value in transfer list is not transferable",
      ));
    }
    transferred_host_objects.push(object);
    prepared.push(PreparedTransfer::HostObject(object));
  }

  let serialized = structured_serialize_internal_with_transfers(
    scope,
    context,
    value,
    false,
    host_objects,
    &transferred_array_buffers,
    &transferred_host_objects,
  )?;

  // A pending V8 exception is represented by the existing empty-buffer
  // sentinel. In particular, do not detach anything on that path.
  if serialized.is_empty() {
    return Ok(StructuredSerializeWithTransferResult {
      serialized,
      transfer_data_holders: Vec::new(),
    });
  }

  let mut transfer_data_holders = Vec::with_capacity(prepared.len());
  for transferable in prepared {
    match transferable {
      PreparedTransfer::ArrayBuffer(array_buffer) => {
        if array_buffer.was_detached() || !array_buffer.is_detachable() {
          return Err(data_clone_error(
            "ArrayBuffer became detached or non-transferable while serializing",
          ));
        }
        let backing_store = array_buffer.get_backing_store();
        if array_buffer.detach(None) != Some(true) {
          return Err(data_clone_error("ArrayBuffer could not be detached"));
        }
        transfer_data_holders
          .push(StructuredCloneTransferData::ArrayBuffer(backing_store));
      }
      PreparedTransfer::HostObject(object) => {
        if !host_objects.validate_transferable_host_object(scope, object)? {
          return Err(data_clone_error(
            "Host object became detached while serializing",
          ));
        }
        let data = host_objects.transfer_host_object(scope, object)?;
        transfer_data_holders
          .push(StructuredCloneTransferData::HostObject(data));
      }
    }
  }

  Ok(StructuredSerializeWithTransferResult {
    serialized,
    transfer_data_holders,
  })
}

fn serialize_v8_graph<'s, 'i, R>(
  scope: &mut v8::PinScope<'s, 'i>,
  context: v8::Local<'s, v8::Context>,
  value: v8::Local<'s, v8::Value>,
  for_storage: bool,
  host_objects: &R,
  transferred_array_buffers: &[(u32, v8::Local<'s, v8::ArrayBuffer>)],
  transferred_host_objects: &[v8::Local<'s, v8::Object>],
) -> Result<Vec<u8>, JsErrorBox>
where
  R: StructuredCloneHostObjectRegistry,
{
  let transferred_host_object_ids = v8::Map::new(scope);
  for (transfer_id, object) in transferred_host_objects.iter().enumerate() {
    let transfer_id = u32::try_from(transfer_id)
      .map_err(|_| data_clone_error("Too many host objects to transfer"))?;
    let transfer_id = v8::Integer::new_from_unsigned(scope, transfer_id);
    if transferred_host_object_ids
      .set(scope, (*object).into(), transfer_id.into())
      .is_none()
    {
      return Err(data_clone_error("Cannot index host object transfer"));
    }
  }
  let serializer = v8::ValueSerializer::new(
    scope,
    Box::new(V8SerializerDelegate {
      _for_storage: for_storage,
      host_objects,
      context: v8::Global::new(scope, context),
      transferred_host_object_ids: v8::Global::new(
        scope,
        transferred_host_object_ids,
      ),
    }),
  );
  serializer.write_raw_bytes(EMBEDDER_MAGIC);
  serializer.write_uint32(host_objects.wire_format_version());
  serializer.write_header();
  for (transfer_id, array_buffer) in transferred_array_buffers {
    serializer.transfer_array_buffer(*transfer_id, *array_buffer);
  }

  v8::tc_scope!(let tc_scope, scope);
  let written = serializer.write_value(context, value);
  if tc_scope.has_caught() || tc_scope.has_terminated() {
    tc_scope.rethrow();
    // The pending V8 exception is rethrown by the op dispatcher.
    return Ok(vec![]);
  }
  if written != Some(true) {
    return Err(JsErrorBox::type_error("Failed to serialize value"));
  }

  Ok(serializer.release())
}

// https://html.spec.whatwg.org/multipage/structured-data.html#structureddeserialize
pub fn structured_deserialize<'s, 'i, R>(
  scope: &mut v8::PinScope<'s, 'i>,
  serialized: Vec<u8>,
  target_realm: v8::Local<'s, v8::Context>,
  host_objects: &R,
) -> Result<v8::Local<'s, v8::Value>, JsErrorBox>
where
  R: StructuredCloneHostObjectRegistry,
{
  deserialize_v8_graph(scope, target_realm, &serialized, host_objects, &[], &[])
}

// https://html.spec.whatwg.org/multipage/structured-data.html#structureddeserializewithtransfer
pub fn structured_deserialize_with_transfer<'s, 'i, R>(
  scope: &mut v8::PinScope<'s, 'i>,
  result: StructuredSerializeWithTransferResult<R::TransferData>,
  target_realm: v8::Local<'s, v8::Context>,
  host_objects: &R,
) -> Result<StructuredDeserializeWithTransferResult<'s>, JsErrorBox>
where
  R: StructuredCloneHostObjectRegistry,
{
  let mut transferred_values =
    Vec::with_capacity(result.transfer_data_holders.len());
  let mut transferred_array_buffers = Vec::new();
  let mut transferred_host_objects = Vec::new();

  for holder in result.transfer_data_holders {
    match holder {
      StructuredCloneTransferData::ArrayBuffer(backing_store) => {
        let array_buffer =
          v8::ArrayBuffer::with_backing_store(scope, &backing_store);
        let transfer_id = transferred_array_buffers.len() as u32;
        transferred_array_buffers.push((transfer_id, array_buffer));
        transferred_values.push(array_buffer.into());
      }
      StructuredCloneTransferData::HostObject(data) => {
        let object = host_objects.receive_host_object(scope, data)?;
        transferred_host_objects.push(object);
        transferred_values.push(object.into());
      }
    }
  }

  let deserialized = deserialize_v8_graph(
    scope,
    target_realm,
    &result.serialized,
    host_objects,
    &transferred_array_buffers,
    &transferred_host_objects,
  )?;

  Ok(StructuredDeserializeWithTransferResult {
    deserialized,
    transferred_values,
  })
}

fn deserialize_v8_graph<'s, 'i, R>(
  scope: &mut v8::PinScope<'s, 'i>,
  target_realm: v8::Local<'s, v8::Context>,
  bytes: &[u8],
  host_objects: &R,
  transferred_array_buffers: &[(u32, v8::Local<'s, v8::ArrayBuffer>)],
  transferred_host_objects: &[v8::Local<'s, v8::Object>],
) -> Result<v8::Local<'s, v8::Value>, JsErrorBox>
where
  R: StructuredCloneHostObjectRegistry,
{
  let (wire_format_version, bytes) = read_embedder_envelope(bytes)?;
  if wire_format_version == 0
    || wire_format_version > host_objects.wire_format_version()
  {
    return Err(JsErrorBox::range_error(format!(
      "Unsupported structured clone wire format version {wire_format_version}"
    )));
  }
  let deserializer = v8::ValueDeserializer::new(
    scope,
    Box::new(V8DeserializerDelegate {
      host_objects,
      target_realm: v8::Global::new(scope, target_realm),
      wire_format_version,
      transferred_host_objects: transferred_host_objects
        .iter()
        .map(|object| v8::Global::new(scope, *object))
        .collect(),
    }),
    bytes,
  );
  if !deserializer.read_header(target_realm).unwrap_or_default() {
    return Err(JsErrorBox::range_error("Cannot deserialize value header"));
  }
  for (transfer_id, array_buffer) in transferred_array_buffers {
    deserializer.transfer_array_buffer(*transfer_id, *array_buffer);
  }
  deserializer
    .read_value(target_realm)
    .ok_or_else(|| JsErrorBox::range_error("Cannot read deserialize value"))
}

fn read_embedder_envelope(bytes: &[u8]) -> Result<(u32, &[u8]), JsErrorBox> {
  if !bytes.starts_with(EMBEDDER_MAGIC) {
    return Err(JsErrorBox::range_error(
      "Cannot deserialize structured clone magic",
    ));
  }

  let mut version = 0u32;
  for index in 0..U32_VARINT_MAX_BYTES {
    let byte = *bytes.get(EMBEDDER_MAGIC.len() + index).ok_or_else(|| {
      JsErrorBox::range_error("Cannot deserialize structured clone version")
    })?;
    let value = byte & VARINT_VALUE_MASK;
    if index == U32_VARINT_MAX_BYTES - 1
      && (value > U32_VARINT_LAST_BYTE_MAX
        || byte & VARINT_CONTINUATION_BIT != 0)
    {
      break;
    }
    version |= (value as u32) << (index * VARINT_VALUE_BITS_PER_BYTE);
    if byte & VARINT_CONTINUATION_BIT == 0 {
      return Ok((version, &bytes[EMBEDDER_MAGIC.len() + index + 1..]));
    }
  }

  Err(JsErrorBox::range_error(
    "Cannot deserialize structured clone version",
  ))
}

static TRANSFER_STR: crate::FastStaticString = crate::ascii_str!("transfer");

pub struct StructuredSerializeOptions<'s> {
  pub transfer: Vec<v8::Local<'s, v8::Value>>,
}

impl<'s> StructuredSerializeOptions<'s> {
  pub fn convert<'i>(
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
      Vec::<v8::Local<'s, v8::Value>>::convert(
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

#[cfg(test)]
mod tests {
  use super::EMBEDDER_MAGIC;
  use super::read_embedder_envelope;

  #[test]
  fn reads_embedder_envelope() {
    let payload = [0xFF, 0x0F];
    let bytes = [EMBEDDER_MAGIC.as_slice(), &[1], &payload].concat();
    let (version, remaining) = read_embedder_envelope(&bytes).unwrap();
    assert_eq!(version, 1);
    assert_eq!(remaining, payload);

    let bytes =
      [EMBEDDER_MAGIC.as_slice(), &[0xAC, 0x02], &payload[..1]].concat();
    let (version, remaining) = read_embedder_envelope(&bytes).unwrap();
    assert_eq!(version, 300);
    assert_eq!(remaining, &payload[..1]);
  }

  #[test]
  fn rejects_invalid_embedder_envelope() {
    assert!(read_embedder_envelope(&[]).is_err());
    assert!(read_embedder_envelope(EMBEDDER_MAGIC).is_err());
    let invalid_version =
      [EMBEDDER_MAGIC.as_slice(), &[0x80, 0x80, 0x80, 0x80, 0x10]].concat();
    assert!(read_embedder_envelope(&invalid_version).is_err());
  }
}
