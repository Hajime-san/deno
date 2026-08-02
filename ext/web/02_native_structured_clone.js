// Copyright 2018-2026 the Deno authors. MIT license.

(function () {
const { op_native_structured_clone } = __bootstrap.core.ops;

function structuredClone(value, options) {
  return op_native_structured_clone(value, options);
}

return { structuredClone };
})();
