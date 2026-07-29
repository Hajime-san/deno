// Copyright 2018-2026 the Deno authors. MIT license.

use std::collections::HashMap;

use deno_core::v8;
use deno_core::v8::ValueDeserializerHelper;
use deno_core::v8::ValueSerializerHelper;
use deno_error::JsErrorBox;

#[derive(PartialEq)]
pub enum SerializedValue<'s> {
  Primitive(v8::Local<'s, v8::Value>),
  Object(Vec<u8>),
}

struct ValueSerializer;

impl v8::ValueSerializerImpl for ValueSerializer {
  fn throw_data_clone_error<'s>(
    &self,
    scope: &mut v8::PinScope<'s, '_>,
    message: v8::Local<'s, v8::String>,
  ) {
    let error = v8::Exception::error(scope, message);
    scope.throw_exception(error);
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

struct ValueDeserializer;

impl v8::ValueDeserializerImpl for ValueDeserializer {}

// https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializeinternal
pub fn structured_serialize_internal<'s, 'i>(
  scope: &mut v8::PinScope<'s, 'i>,
  context: v8::Local<'s, v8::Context>,
  value: v8::Local<'s, v8::Value>,
  for_storage: bool,
  memory: &mut HashMap<v8::Local<v8::Value>, u32>,
) -> Result<SerializedValue<'s>, JsErrorBox> {
  // 1.
  // 2.
  // if memory.contains_key(&value) {
  //   return Ok(SerializedValue::Primitive(value));
  // }

  // 3.
  let mut deep = false;

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

  // 6.
  let serializer =
    v8::ValueSerializer::new(scope, Box::new(ValueSerializer {}));
  let mut serialized = vec![];

  if value.is_object() {
    if
    // 7.
    value.is_boolean_object()
    // 8.
    || value.is_number_object()
    // 9.
    || value.is_big_int_object()
    // 10.
    || value.is_string_object()
    // 11.
    || value.is_date()
    // 12.
    || value.is_reg_exp()
    {
      serializer.write_header();
      serializer.write_value(context, value);
      let mut binary_value = serializer.release();
      serialized.append(&mut binary_value);
    }

    // 13.

    // 1.
    if value.is_shared_array_buffer() {
      // 1. skip
      // 2.
      if for_storage {
        return Err(JsErrorBox::new(
          "DataCloneError",
          "Cannot serialize SharedArrayBuffer for storage",
        ));
      }
      // 3.
      // 4.
      // let shared_array_buffer = v8::Local::<v8::SharedArrayBuffer>::try_from(value)?;
      // let backing_store = shared_array_buffer.get_backing_store();
      // if !backing_store.is_resizable_by_user_javascript() {
      // }
      serializer.write_header();
      serializer.write_value(context, value);
      let mut binary_value = serializer.release();
      serialized.append(&mut binary_value);
    }
    // 2.
    else {
      // 1.
      if let Ok(array_buffer) = v8::Local::<v8::ArrayBuffer>::try_from(value) {
        if array_buffer.is_detachable() {
          return Err(JsErrorBox::new(
            "DataCloneError",
            "Cannot serialize detached ArrayBuffer",
          ));
        }
      }
    }
  }

  Ok(SerializedValue::Object(serialized))
}

// https://html.spec.whatwg.org/multipage/structured-data.html#structureddeserialize
pub fn structured_deserialize<'s, 'i>(
  scope: &mut v8::PinScope<'s, 'i>,
  serialized: SerializedValue<'s>,
  target_realm: v8::Local<'s, v8::Context>,
  memory: &mut HashMap<v8::Local<v8::Value>, u32>,
) -> Result<v8::Local<'s, v8::Value>, JsErrorBox> {
  // 1.
  // 2.
  // if memory.contains_key(&serialized) {
  //   return Ok(serialized);
  // }
  // 3.
  let mut deep = false;
  // 4.
  let value = match serialized {
    // 5.
    SerializedValue::Primitive(serialized) => serialized,
    SerializedValue::Object(obj) => {
      let value_deserializer =
        v8::ValueDeserializer::new(scope, Box::new(ValueDeserializer {}), &obj);
      let parsed_header = value_deserializer
        .read_header(scope.get_current_context())
        .unwrap_or_default();
      if !parsed_header {
        return Err(JsErrorBox::range_error("Cannot deserialize value header"));
      }
      let Some(value) =
        value_deserializer.read_value(scope.get_current_context())
      else {
        return Err(JsErrorBox::range_error("Cannot read deserialize value"));
      };

      if
      // 6.
      value.is_boolean_object()
      // 7.
      || value.is_number_object()
      // 8.
      || value.is_big_int_object()
      // 9.
      || value.is_string_object()
      // 10.
      || value.is_date()
      // 11.
      || value.is_reg_exp()
      {
        return Ok(value);
      }

      if value.is_shared_array_buffer() {
        // 12.

        // 13.
        // let shared_array_buffer = v8::Local::<v8::SharedArrayBuffer>::try_from(value)?;
        // let backing_store = shared_array_buffer.get_backing_store();
        // if backing_store.is_resizable_by_user_javascript() {

        // }
        return Ok(value);
      }

      unreachable!()
    }
  };

  Ok(value.into())
}
