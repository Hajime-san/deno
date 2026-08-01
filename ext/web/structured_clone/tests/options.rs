#[test]
fn converts_structured_serialize_options_in_rust() {
  let mut runtime = runtime();
  runtime
      .execute_script(
        "structured_clone_options_conversion.js",
        r#"
          const { structuredClone } = Deno.core.loadExtScript(
            "ext:deno_web/02_structured_clone.js",
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
use super::*;
