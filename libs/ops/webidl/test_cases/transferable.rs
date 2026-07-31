// Copyright 2018-2026 the Deno authors. MIT license.

#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_ops::webidl;

#[webidl(transferable)]
pub struct TransferableInterface;

pub fn assert_transferable_marker()
where
  TransferableInterface: deno_core::WebIdlTransferable,
{
}
