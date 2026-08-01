#[test]
fn transfers_array_buffer() {
  let mut runtime = runtime();
  runtime
      .execute_script(
        "structured_clone_array_buffer_transfer.js",
        r#"
          const { structuredClone } = Deno.core.loadExtScript(
            "ext:deno_web/02_structured_clone.js",
          );
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
          const { structuredClone } = Deno.core.loadExtScript(
            "ext:deno_web/02_structured_clone.js",
          );
          const duplicate = new ArrayBuffer(4);
          let duplicateThrew = false;
          try {
            structuredClone(null, { transfer: [duplicate, duplicate] });
          } catch {
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
use super::*;
