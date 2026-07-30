// Copyright 2018-2026 the Deno authors. MIT license.

use deno_core::v8;
use deno_core::v8::ValueDeserializerHelper;
use deno_core::v8::ValueSerializerHelper;
use deno_error::JsErrorBox;

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

pub enum SerializedValue<'s> {
  Primitive(v8::Local<'s, v8::Value>),
  V8(Vec<u8>),
}

/// Hooks for platform objects whose serialization is defined by the embedder.
///
/// The hooks are called by V8 while it walks a single object graph, so they
/// must write and read exactly one host-object record per invocation.
pub trait StructuredCloneHostObjectRegistry {
  fn is_host_object<'s, 'i>(
    &self,
    scope: &mut v8::PinScope<'s, 'i>,
    object: v8::Local<'s, v8::Object>,
  ) -> bool;

  fn write_host_object<'s, 'i>(
    &self,
    scope: &mut v8::PinScope<'s, 'i>,
    object: v8::Local<'s, v8::Object>,
    serializer: &dyn v8::ValueSerializerHelper,
  ) -> Option<bool>;

  fn read_host_object<'s, 'i>(
    &self,
    scope: &mut v8::PinScope<'s, 'i>,
    deserializer: &dyn v8::ValueDeserializerHelper,
  ) -> Option<v8::Local<'s, v8::Object>>;
}

// V8 owns reference tracking while it serializes a complete object graph.
// Keep Web(Deno) platform specific state here as support is added.
struct V8SerializerDelegate<'a, R> {
  _for_storage: bool,
  host_objects: &'a R,
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
    self
      .host_objects
      .write_host_object(scope, object, serializer)
  }

  // fn get_shared_array_buffer_id<'s, 'i>(
  //   &self,
  //   scope: &mut v8::PinScope<'s, 'i>,
  //   shared_array_buffer: v8::Local<'s, v8::SharedArrayBuffer>,
  // ) -> Option<u32> {
  //   // Broadcast mode: carry the backing store out-of-band and use its index in
  //   // the list as the transfer id.
  //   if let Some(broadcast) = &self.broadcast_shared_array_buffers {
  //     let backing_store = shared_array_buffer.get_backing_store();
  //     let mut list = broadcast.borrow_mut();
  //     let id = list.len() as u32;
  //     list.push(backing_store);
  //     return Some(id);
  //   }
  //   if self.for_storage {
  //     return None;
  //   }
  //   let state = JsRuntime::state_from(scope);
  //   match &state.shared_array_buffer_store {
  //     Some(shared_array_buffer_store) => {
  //       let backing_store = shared_array_buffer.get_backing_store();
  //       let id = shared_array_buffer_store.insert(backing_store);
  //       Some(id)
  //     }
  //     _ => None,
  //   }
  // }
}

struct V8DeserializerDelegate<'a, R> {
  host_objects: &'a R,
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
    self.host_objects.read_host_object(scope, deserializer)
  }
}

// https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal
pub fn structured_serialize_internal<'s, 'i, R>(
  scope: &mut v8::PinScope<'s, 'i>,
  context: v8::Local<'s, v8::Context>,
  value: v8::Local<'s, v8::Value>,
  for_storage: bool,
  host_objects: &R,
) -> Result<SerializedValue<'s>, JsErrorBox>
where
  R: StructuredCloneHostObjectRegistry,
{
  // 4.
  if value.is_undefined()
    || value.is_null()
    || value.is_boolean()
    || value.is_number()
    || value.is_big_int()
    || value.is_string()
  {
    return Ok(SerializedValue::Primitive(value));
  }

  // 5.
  if value.is_symbol() {
    return Err(JsErrorBox::new("DataCloneError", "Cannot serialize Symbol"));
  }

  // 6.~
  // V8 owns the recursive object graph traversal, including reference tracking
  // for aliases and cycles. Do not invoke this backend recursively per type.
  serialize_v8_graph(scope, context, value, for_storage, host_objects)
}

fn serialize_v8_graph<'s, 'i, R>(
  scope: &mut v8::PinScope<'s, 'i>,
  context: v8::Local<'s, v8::Context>,
  value: v8::Local<'s, v8::Value>,
  for_storage: bool,
  host_objects: &R,
) -> Result<SerializedValue<'s>, JsErrorBox>
where
  R: StructuredCloneHostObjectRegistry,
{
  let serializer = v8::ValueSerializer::new(
    scope,
    Box::new(V8SerializerDelegate {
      _for_storage: for_storage,
      host_objects,
    }),
  );
  serializer.write_header();

  v8::tc_scope!(let tc_scope, scope);
  let written = serializer.write_value(context, value);
  if tc_scope.has_caught() || tc_scope.has_terminated() {
    tc_scope.rethrow();
    // The pending V8 exception is rethrown by the op dispatcher.
    return Ok(SerializedValue::V8(vec![]));
  }
  if written != Some(true) {
    return Err(JsErrorBox::type_error("Failed to serialize value"));
  }

  Ok(SerializedValue::V8(serializer.release()))
}

// https://html.spec.whatwg.org/multipage/structured-data.html#structureddeserialize
pub fn structured_deserialize<'s, 'i, R>(
  scope: &mut v8::PinScope<'s, 'i>,
  serialized: SerializedValue<'s>,
  target_realm: v8::Local<'s, v8::Context>,
  host_objects: &R,
) -> Result<v8::Local<'s, v8::Value>, JsErrorBox>
where
  R: StructuredCloneHostObjectRegistry,
{
  // 4.
  match serialized {
    // 5.
    SerializedValue::Primitive(value) => Ok(value),
    // 6.~
    SerializedValue::V8(bytes) => {
      deserialize_v8_graph(scope, target_realm, &bytes, host_objects)
    }
  }
}

fn deserialize_v8_graph<'s, 'i, R>(
  scope: &mut v8::PinScope<'s, 'i>,
  target_realm: v8::Local<'s, v8::Context>,
  bytes: &[u8],
  host_objects: &R,
) -> Result<v8::Local<'s, v8::Value>, JsErrorBox>
where
  R: StructuredCloneHostObjectRegistry,
{
  let deserializer = v8::ValueDeserializer::new(
    scope,
    Box::new(V8DeserializerDelegate { host_objects }),
    bytes,
  );
  if !deserializer.read_header(target_realm).unwrap_or_default() {
    return Err(JsErrorBox::range_error("Cannot deserialize value header"));
  }
  deserializer
    .read_value(target_realm)
    .ok_or_else(|| JsErrorBox::range_error("Cannot read deserialize value"))
}
