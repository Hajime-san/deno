// Copyright 2018-2026 the Deno authors. MIT license.

use deno_core::op2;
use deno_core::structured_deserialize;
use deno_core::structured_serialize_internal;
use deno_core::v8;
use deno_error::JsErrorBox;

// https://html.spec.whatwg.org/multipage/structured-data.html#dom-structuredclone
#[op2]
pub fn structured_clone<'s, 'i>(
  scope: &mut v8::PinScope<'s, 'i>,
  value: v8::Local<'s, v8::Value>,
  _options: Option<v8::Local<'s, v8::Object>>,
) -> Result<v8::Local<'s, v8::Value>, JsErrorBox> {
  let context = scope.get_current_context();

  let serialized = structured_serialize_internal(scope, context, value, false)?;
  let deserialize = structured_deserialize(scope, serialized, context)?;

  Ok(deserialize)

  // let context = scope.get_current_context();
  // v8::tc_scope!(tc_scope, scope);

  // if tc_scope.has_caught() {
  //   return Ok(v8::Local::new(tc_scope, value));
  // }
}
