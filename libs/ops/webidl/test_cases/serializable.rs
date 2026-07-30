// Copyright 2018-2026 the Deno authors. MIT license.

#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_ops::webidl;

#[webidl(serializable)]
pub struct SerializableInterface;

pub fn assert_serializable_marker()
where
  SerializableInterface: deno_core::WebIdlSerializable,
{
}
