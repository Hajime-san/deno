// Copyright 2018-2026 the Deno authors. MIT license.

#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_ops::webidl;

#[webidl(serializable, transferable)]
pub struct SerializableTransferableInterface;

pub fn assert_combined_markers()
where
  SerializableTransferableInterface:
    deno_core::WebIdlSerializable + deno_core::WebIdlTransferable,
{
}
