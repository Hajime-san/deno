// Copyright 2018-2026 the Deno authors. MIT license.

#[cfg(test)]
use std::sync::OnceLock;

use deno_core::SerializationTag;
#[cfg(test)]
use deno_core::StructuredCloneRegistry;
#[cfg(test)]
use deno_core::StructuredDeserializeWithTransferResult;
#[cfg(test)]
use deno_core::op2;
#[cfg(test)]
use deno_core::structured_deserialize_with_transfer;
#[cfg(test)]
use deno_core::structured_serialize_with_transfer;
use deno_core::v8;
use deno_error::JsErrorBox;

use crate::image_data::ImageData;

#[cfg(test)]
fn create_web_structured_clone_registry() -> StructuredCloneRegistry {
  let mut registry = StructuredCloneRegistry::new();
  registry
    .register_serializable::<ImageData>(SerializationTag::ImageData as u8);
  registry
}

#[cfg(test)]
static WEB_STRUCTURED_CLONE_HOST_OBJECT_REGISTRY: OnceLock<
  StructuredCloneRegistry,
> = OnceLock::new();

#[cfg(test)]
fn web_structured_clone_host_object_registry()
-> &'static StructuredCloneRegistry {
  WEB_STRUCTURED_CLONE_HOST_OBJECT_REGISTRY
    .get_or_init(create_web_structured_clone_registry)
}

// https://html.spec.whatwg.org/multipage/structured-data.html#dom-structuredclone
#[cfg(test)]
#[op2]
pub fn op_native_structured_clone<'s, 'i>(
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
fn register_data_clone_error_builder(runtime: &mut deno_core::JsRuntime) {
  runtime
    .execute_script(
      "structured_clone_error_builder.js",
      r#"
        (() => {
          const { DOMException } = Deno.core.loadExtScript(
            "ext:deno_web/01_dom_exception.js",
          );
          Deno.core.registerErrorBuilder(
            "DOMExceptionDataCloneError",
            (message) => new DOMException(message, "DataCloneError"),
          );
        })();
      "#,
    )
    .unwrap();
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
    let mut runtime = JsRuntime::new(RuntimeOptions {
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
    });
    super::register_data_clone_error_builder(&mut runtime);
    runtime
  }

  #[test]
  fn transfers_array_buffer() {
    let mut runtime = runtime();
    runtime
        .execute_script(
          "structured_clone_array_buffer_transfer.js",
          r#"
            const { structuredClone } = Deno.core.loadExtScript(
              "ext:deno_web/02_native_structured_clone.js",
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
            const { DOMException } = Deno.core.loadExtScript(
              "ext:deno_web/01_dom_exception.js",
            );
            const { structuredClone } = Deno.core.loadExtScript(
              "ext:deno_web/02_native_structured_clone.js",
            );
            const duplicate = new ArrayBuffer(4);
            let duplicateThrew = false;
            try {
              structuredClone(null, { transfer: [duplicate, duplicate] });
            } catch (error) {
              if (!(error instanceof DOMException) || error.name !== "DataCloneError") {
                throw error;
              }
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
    let mut runtime = JsRuntime::new(RuntimeOptions {
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
    });
    super::register_data_clone_error_builder(&mut runtime);
    runtime
  }

  #[test]
  fn converts_structured_serialize_options_in_rust() {
    let mut runtime = runtime();
    runtime
        .execute_script(
          "structured_clone_options_conversion.js",
          r#"
            const { structuredClone } = Deno.core.loadExtScript(
              "ext:deno_web/02_native_structured_clone.js",
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
    let mut runtime = JsRuntime::new(RuntimeOptions {
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
    });
    super::register_data_clone_error_builder(&mut runtime);
    runtime
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
            "ext:deno_web/02_native_structured_clone.js",
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
            "ext:deno_web/02_native_structured_clone.js",
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
