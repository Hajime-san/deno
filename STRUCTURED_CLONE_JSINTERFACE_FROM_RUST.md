# JS Host Object を Rust の Structured Clone に橋渡しする

## 目的

JavaScript で実装されている Web IDL インターフェースを、Rust 側の
structured-clone 実装で識別し、必要に応じて native host object として
serialize/deserialize できるようにする。

ここでいう「識別」は、`instanceof` や prototype chain ではなく、Deno の
信頼された実装が生成した object であることを検証することを指す。
`Object.create(Blob.prototype)` のような prototype を偽装した object は
受け入れない。

## 実装済みの private brand bridge

`libs/core/ops_builtin_types.rs` に、V8 private property を使う汎用 bridge を
追加した。

```rust
pub fn is_branded(
  scope: &mut v8::PinScope,
  value: v8::Local<v8::Value>,
  brand: &str,
) -> bool

#[op2(nofast)]
pub fn op_mark_branded(
  scope: &mut v8::PinScope,
  object: v8::Local<v8::Object>,
  #[string] brand: &str,
) -> Result<(), JsErrorBox>
```

`op_mark_branded(object, "MyHostObject")` は、名前
`Deno.core.privateBrand.MyHostObject` から `v8::Private::for_api()` で取得した
private key に `true` を設定する。`is_branded()` は同じ key を使って値を検査する。

V8 private property は通常の JavaScript から列挙、取得、設定できない。そのため、
private key 自体を持たない JavaScript は prototype だけを真似しても brand を複製
できない。

```js
class MyHostObject {
  constructor(value) {
    this.value = value;
    Deno.core.ops.op_mark_branded(this, "MyHostObject");
  }
}

const value = new MyHostObject("contents");
const forged = Object.create(MyHostObject.prototype);
```

```rust
assert!(deno_core::is_branded(scope, value, "MyHostObject"));
assert!(!deno_core::is_branded(scope, forged, "MyHostObject"));
```

この検証は
`ext/web/structured_clone/structured_clone.rs` の
`identifies_my_host_object_before_native_structured_clone` テストで行っている。
同テストの native clone op は、brand を検出した場合に debug log を出すだけで、
`MyHostObject` 専用の serialize handler はまだ登録していない。

### 信頼境界

private property であることだけでは、`op_mark_branded` を呼べる JavaScript に対して
brand を偽装できないことは保証しない。したがって、この op は Deno の信頼された
extension 初期化コードだけが利用できるものとして扱う必要がある。任意の非信頼
JavaScript がこの op を呼べる設計では、brand はセキュリティ境界にはならない。

`brand` には extension が定義する静的な名前だけを使う。`v8::Private::for_api()` は
isolate の生存期間中、名前ごとに key を保持するため、外部入力をそのまま渡して
無制限にブランド名を作ってはならない。

現実装では key 名の生成に `format!` と `v8::String::new()` を使用している。
interface descriptor を静的にできる段階で `FastStaticString` に移行する TODO を
コードに置いている。

## 現在の Blob structured clone

`Blob` は JavaScript 実装であり、`type`、`size`、part の配列、file-backed 状態を
`09_file.js` の module-private Symbol に持つ。Rust が `Blob` brand を検出できても、
この Symbol の値は直接取得できない。

現行の production path は Rust の host-object registry を通らない。

1. `Blob.prototype[core.hostObjectBrand]` が `type: "Blob"`、MIME type、part、size
   を持つ metadata object を返す。
2. `cloneBlobParts()` が入れ子の `Blob` part を flatten し、各 part に対して
   `op_blob_clone_part()` を実行する。file-backed Blob はここで clone を拒否する。
3. `core.registerCloneableResource("Blob", ...)` が metadata から新しい JavaScript
   `Blob` を作り、module-private Symbol の状態を復元する。

このため `Blob` に `op_mark_branded(this, "Blob")` を追加しても、それだけで Rust が
Blob を clone できるようにはならない。brand は dispatch に使えるが、payload を書く
ための Blob state は別途 Rust から読める必要がある。

`File extends Blob` も考慮が必要である。`File` instance は `Blob` としても brand され
得る一方、clone payload には `name` と `lastModified` が追加で必要である。複数の
brand が一致する場合、registry は最も具体的な interface を優先して dispatch する
必要がある。

## Native registry との接続点

`WebStructuredCloneHostObjectRegistry` は現在、CppGC wrapper に格納された Rust
`TypeId` で handler を選択する。`register_serializable::<T>()` は
`StructuredCloneHostObject + GarbageCollected` を要求するため、通常の JavaScript
`Blob` をそのまま登録する API ではない。

private-brand の JS host object を扱うには、CppGC handler と別に次のような descriptor
登録経路を追加する。

```rust
registry.register_js_serializable(
  SerializationTag::Blob as u8,
  "Blob",
  write_blob_payload,
  read_blob_payload,
);
```

概念上、registry は tag から read handler を引く表に加え、brand から write handler
を引く表を持つ。

```text
serialize: object --is_branded("Blob")--> Blob write handler --tag + payload-->
deserialize: tag ------------------------> Blob read handler ---------------> object
```

`SerializationTag` は永続的な wire identifier である。Blob 用 tag を追加
する場合は値を明示して予約し、既存の tag を再利用・再採番しない。payload の互換性
方針と fixture は `libs/core/serialization/README.md` に従う。

## Blob を Rust handler で扱う選択肢

### 1. JS metadata を利用する段階的な bridge

Rust の `write_blob_payload` は既存の Blob host-object callback と同じ metadata を
JavaScript から取得し、tag の後に V8 serializer で書く。`read_blob_payload` は
metadata を読んで、既存の Blob factory/deserializer を呼ぶ。

利点は、Blob の part flatten、`BlobStore` の clone、file-backed rejection、JS private
Symbol の初期化を再実装しないことにある。欠点は serialize/deserialize 中に
JavaScript を呼ぶことである。呼び出し元 op は reentrant でなければならず、例外を
V8 の例外として正しく伝播させる必要がある。

これは Rust が host object の dispatch と wire format を所有しつつ、Blob 固有の状態
変換は JavaScript に残す方式である。

### 2. Rust-visible metadata を Blob と同期する

Blob の生成時に、次のような Rust から読める metadata を登録する。

```text
BlobCloneData {
  media_type: String,
  parts: Vec<{ uuid, size }>,
  size: usize,
  file_backed: bool,
}
```

write handler は `BlobStore` を取得し、各 UUID に対して現在の
`op_blob_clone_part()` と同等の clone を行い、payload を直接書く。

この方式では metadata と JavaScript の Symbol state を常に同期しなければならない。
少なくとも constructor、`slice()`、object URL からの復元、structured-clone の復元、
file-backed 化のすべてで更新が必要になる。Blob の method は現在も JavaScript の
Symbol state を読むため、deserialize で有効な JavaScript Blob を作るには JavaScript
factory を呼ぶか、Blob の状態管理そのものを書き換える必要がある。

### 3. Blob を CppGC host object に移行する

Blob state を CppGC wrapper の Rust struct に移し、`StructuredCloneHostObject` を実装
する。これは `ImageData` と同じ registry 経路を使えるため、最も一貫した native
モデルになる。

ただし、Web IDL wrapper、`File` との継承、part lifetime、BlobStore との接続、および
JavaScript API の実装を移行する大きな変更である。private-brand bridge はこの移行前に
通常の JavaScript host object を段階的に取り込むための仕組みとして有用である。

## 実装時の確認項目

- `Blob` と `File` を別々の permanent wire tag とし、最も具体的な handler を選ぶ。
- file-backed Blob の `DataCloneError` 相当の既存挙動を維持する。
- 同じ Blob が object graph 内で複数回現れる場合の identity を V8 serializer に任せ、
  part clone が重複して実行されないことをテストする。
- Blob part の clone が `BlobStore` に対して正しく行われ、clone 前後で source Blob が
  読み出せることをテストする。
- private brand を持つ偽装 prototype object が handler に入らないことをテストする。
- 新しい host-object payload を永続化する場合は wire-format fixture と後方互換 decode
  test を追加する。
