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
use super::*;
