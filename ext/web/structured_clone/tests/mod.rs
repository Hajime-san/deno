use std::sync::Arc;

use deno_core::JsRuntime;
use deno_core::RuntimeOptions;
use deno_core::is_structured_clone_host_object;
use deno_core::v8;

use super::WebStructuredCloneHostObjectRegistry;
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

mod array_buffer;
mod host_object_transfer;
mod image_data;
mod options;
mod serialization;
mod test_transferable;
