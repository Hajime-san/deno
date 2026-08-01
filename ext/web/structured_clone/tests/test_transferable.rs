use std::cell::Cell;

use deno_core::GarbageCollected;
use deno_core::StructuredCloneTransferable;
use deno_core::v8;
use deno_error::JsErrorBox;

pub(super) struct TestTransferable {
  pub value: u32,
  pub detached: Cell<bool>,
}

// SAFETY: TestTransferable contains no references requiring GC tracing.
unsafe impl GarbageCollected for TestTransferable {
  fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}

  fn get_name(&self) -> &'static std::ffi::CStr {
    <Self as deno_core::WebIdlInterface>::INTERFACE_NAME
  }
}

impl StructuredCloneTransferable for TestTransferable {
  type TransferData = u32;

  fn validate_transfer(&self) -> Result<(), JsErrorBox> {
    if self.detached.get() {
      return Err(JsErrorBox::new(
        "DOMExceptionDataCloneError",
        "TestTransferable is detached",
      ));
    }
    Ok(())
  }

  fn transfer<'s, 'i>(
    &self,
    _scope: &mut v8::PinScope<'s, 'i>,
  ) -> Result<Self::TransferData, JsErrorBox> {
    self.detached.set(true);
    Ok(self.value)
  }

  fn receive<'s, 'i>(
    _scope: &mut v8::PinScope<'s, 'i>,
    value: Self::TransferData,
  ) -> Result<Self, JsErrorBox> {
    Ok(Self {
      value,
      detached: Cell::new(false),
    })
  }
}

impl deno_core::WebIdlInterface for TestTransferable {
  const INTERFACE_NAME: &'static std::ffi::CStr = c"TestTransferable";
}

impl deno_core::WebIdlTransferable for TestTransferable {}
