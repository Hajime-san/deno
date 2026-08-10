// Copyright 2018-2026 the Deno authors. MIT license.

use std::sync::Arc;

use deno_core::JsRuntime;
use deno_core::RuntimeOptions;
use deno_core::is_structured_clone_host_object;
use deno_core::v8;

use super::create_web_structured_clone_registry;
use crate::image_data::ImageData;

fn runtime() -> JsRuntime {
  JsRuntime::new(RuntimeOptions {
    extensions: vec![
      deno_webidl::deno_webidl::init(),
      crate::deno_web::init(
        Arc::new(crate::BlobStore::default()) as Arc<dyn crate::BlobStoreTrait>,
        None,
        Default::default(),
        Default::default(),
      ),
    ],
    ..Default::default()
  })
}

fn get_property<'s>(
  scope: &mut v8::PinScope<'s, '_>,
  object: v8::Local<'s, v8::Object>,
  name: &str,
) -> v8::Local<'s, v8::Value> {
  let key = v8::String::new(scope, name).unwrap();
  object.get(scope, key.into()).unwrap()
}

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
  0xFE, // Deno envelope tag
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
  let registry = create_web_structured_clone_registry();
  let value = deno_core::v8::Local::new(scope, value);
  let bytes =
    deno_core::structured_serialize_internal(scope, value, false, &registry)
      .unwrap();

  assert_eq!(bytes, IMAGE_DATA_V1_V8_16);
}
#[test]
fn decodes_image_data_v1_fixture() {
  let mut runtime = runtime();

  deno_core::scope!(scope, runtime);
  let context = scope.get_current_context();
  let registry = create_web_structured_clone_registry();
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
