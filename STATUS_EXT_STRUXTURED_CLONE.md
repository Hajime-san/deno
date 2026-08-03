調査結果として、`Deno.core.serialize/deserialize` は ext/web の global
`structuredClone` と置換可能な同種 API ではありません。したがって「global
`structuredClone` が ext/web 実装だけで足りる」ことを根拠に `ImageData` /
registry を core へ移すのは不十分です。

| 経路                              | 現在の実装                                              | ext/web registry で置換 |
| --------------------------------- | ------------------------------------------------------- | ----------------------- |
| global `structuredClone`          | `ext:deno_web/02_structured_clone.js` → ext/web op      | 可能                    |
| `Deno.core._structuredClone`      | core の `op_structured_clone`                           | 単純置換は不可          |
| `Deno.core.serialize/deserialize` | core の `op_serialize` / `op_deserialize`               | 不可                    |
| MessagePort                       | `Deno.core.serialize/deserialize`                       | 不可                    |
| BroadcastChannel                  | core の専用 broadcast serializer                        | 不可                    |
| KV 永続化・queue                  | `Deno.core.serialize/deserialize({ forStorage: true })` | 不可                    |

根拠は以下です。

- MessagePort は transfer list の resource を `hostObjects`
  配列として渡し、受信側では `deserializers`
  を渡して復元しています。[13_message_port.js](/Users/hajime_masutani/repository/deno/ext/web/13_message_port.js:630)
  [13_message_port.js](/Users/hajime_masutani/repository/deno/ext/web/13_message_port.js:909)
- KV は永続化用に `forStorage: true`
  を使います。[01_db.ts](/Users/hajime_masutani/repository/deno/ext/kv/01_db.ts:312)
- core serializer は JS の `Deno.core.hostObjectBrand` callback
  を実行し、`registerCloneableResource()` で実行時登録された deserializer
  を使います。[01_core.js](/Users/hajime_masutani/repository/deno/libs/core/01_core.js:722)
  [ops_builtin_v8.rs](/Users/hajime_masutani/repository/deno/libs/core/ops_builtin_v8.rs:616)
- BroadcastChannel は SharedArrayBuffer を複数 receiver 向けに別形式で
  out-of-band
  管理します。[ops_builtin_v8.rs](/Users/hajime_masutani/repository/deno/libs/core/ops_builtin_v8.rs:1033)

特に、ext/web の registry は `ImageData` のような Rust/C++GC host object の
codec を表します。一方 `Deno.core.serialize/deserialize` は、JS
側で動的に登録される cloneable resource、MessagePort resource transfer、KV
storage policy、SAB/Wasm transfer store を扱う transport 基盤です。現在の
registry だけでは後者を表現できません。

このため設計判断は次の二択です。

- `ImageData` の対応範囲を global `structuredClone` のみに限定するなら、registry
  は ext/web に置く方が自然です。今回の core への移動は不要です。
- `ImageData` を MessagePort、BroadcastChannel、KV storage でも clone
  可能にしたいなら、core に codec を置く価値があります。ただし
  `op_serialize/op_deserialize` の JS host-object delegate と WebIDL codec
  registry を合成する設計が必要です。単に `ImageData` と registry を core
  へ移しただけでは、それらの経路は新 registry を使いません。

現状の refactor
は後者へ進むための一部にはなりますが、全経路の統合はまだしていません。
