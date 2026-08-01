use super::super::StructuredCloneHostObjectTag;
use super::test_transferable::TestTransferable;
use super::*;

#[test]
fn resolves_host_object_transfer_reference() {
  let mut runtime = runtime();
  deno_core::scope!(scope, runtime);
  let context = scope.get_current_context();
  let mut registry = WebStructuredCloneHostObjectRegistry::default();
  registry.register_transferable::<TestTransferable>(
    StructuredCloneHostObjectTag::TestTransferable,
  );
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
