#[test]
fn intermediate_serialization_encodes_primitives() {
  let mut runtime = runtime();

  deno_core::scope!(scope, runtime);
  let registry = WebStructuredCloneHostObjectRegistry::default();
  let value: v8::Local<v8::Value> = v8::Integer::new(scope, 42).into();
  let bytes =
    deno_core::structured_serialize_internal(scope, value, false, &registry)
      .unwrap();

  assert!(bytes.starts_with(b"DENO"));
  let target_realm = scope.get_current_context();
  let value =
    deno_core::structured_deserialize(scope, bytes, target_realm, &registry)
      .unwrap();
  assert_eq!(value.int32_value(scope), Some(42));
}
use super::*;
