# kTrailerOffsetTagについて

https://source.chromium.org/search?q=kTrailerOffsetTag&ss=chromium%2Fchromium%2Fsrc

**Blinkでの用途**
Blink の wire format は概ね次の構造です。

```text
FF
version: varint
FE
trailer_offset: u64 big-endian
trailer_size: u32 big-endian
V8 header
V8 payload
trailer
```

serialization 開始時には offset と size をゼロで予約します。host object の書き込み中、`WriteAndRequireInterfaceTag()` が使用した Web IDL interface tag を `TrailerWriter` に記録します。

最後に次の trailer を末尾へ追加します。

```text
A0
interface_count: u32 big-endian
interface_tags: [u8; interface_count]
```

その後、予約していた `trailer_offset` と `trailer_size` を実値で上書きします。trailer がなければ両方ともゼロです。

**Deserializeでの用途**
`TrailerReader::SkipToTrailer()` が offset を使って V8 payload を解析せずに trailer へ直接移動します。

`SerializedScriptValue::CanDeserializeIn()` は trailer に記録された interface tag を調べ、受信 realm がすべての interface を公開しているか確認します。

つまり目的は次です。

> 実際に object graph を deserialize する前に、受信 realm で必要な platform interface が利用可能か判定する。

`ImageData`、`Blob`、`MessagePort`、各種 Stream などが `WriteAndRequireInterfaceTag()` を使用しています。

**Denoでのexposure検査の現状**
Deno では platform object 共通の `StructuredCloneHostObject` trait に、次の API を追加済みである。

```rust
fn is_exposed<'s, 'i>(
  scope: &mut v8::PinScope<'s, 'i>,
  target_realm: v8::Local<'s, v8::Context>,
) -> bool;
```

registry は interface ごとの handler としてこの関数を保持する。現在は次のタイミングで使用している。

- 通常の deserialize では、V8 の再帰処理が serialized host-object tag に到達した時点で検査する。これは StructuredDeserialize の step 22.2 に対応する。
- transfer 付き deserialize では、transfer data から受信側 object を生成する前に検査する。これは StructuredDeserializeWithTransfer の step 4.2 に対応する。

`ImageData::is_exposed()` は現在 `true` を返す。Deno の `ImageData` は Window と Worker の両方に公開されるためである。

この実装だけでは trailer と同じ事前検査にはならない。通常の deserialize では object graph を読み進め、対象の host object に到達して初めて判定される。trailer を実装すると、V8 payload を deserialize する前に必要な interface をまとめて検査できる。

なお、通常の deserialize の非 exposed 経路は現在 plain V8 `Error` を throw しており、仕様が要求する `DataCloneError` DOMException への変換は未実装である。transfer data の受信経路は `DOMExceptionDataCloneError` の `JsErrorBox` を返す。

**Denoでの採用方針**
Deno 独自の次の envelope は直ちに置き換える。

```text
FE | Deno version | V8 header | V8 payload
```

置き換え後は Blink と同じ構造を使用する。

```text
SerializationTag::Version (FF)
Deno wire-format version: varint
SerializationTag::TrailerOffset (FE)
trailer_offset: u64 big-endian
trailer_size: u32 big-endian
V8 header
V8 payload
trailer
```

`SerializationTag` は host-object tag だけではなく、structured-clone wire format で Deno が解釈する tag の共通名前空間とする。このため `ImageData`、`Version`、`TrailerOffset` を同じ enum に含めることは Blink の `SerializationTag` と同じ設計である。各 variant が使用できる位置は wire format の文法によって決まる。

初期実装でも `TrailerOffset` に続く offset と size の領域を確保する。trailer をまだ生成しない場合は、両方をゼロにする。これにより、後から trailer の収集と検査を追加しても envelope の配置を再変更する必要がない。

trailer の実処理を追加するには、以下が必要になる。

- host object の書き込み時に使用した interface tag を収集する `TrailerWriter`
- trailer の追加後に offset/size を big-endian で patch する処理
- offset/size の範囲を検証する `TrailerReader`
- `SerializationTag::TrailerRequiresInterfaces` (`A0`) の読み書き
- trailer の interface tag から対応する `is_exposed` handler を引く registry API
- deserialize 前に必要な interface を検査する処理
- 非 exposed 時に `DataCloneError` DOMException を返す処理
- 新形式に合わせた wire-format fixture

旧 Deno envelope との並行サポートは行わず、新形式へ置き換える前提とする。したがって既存 fixture は新しい envelope に更新する。

検索は `kTrailerOffsetTag` から始め、次の順で追うと全体を把握できます。

```text
kTrailerOffsetTag
TrailerWriter
WriteAndRequireInterfaceTag
kTrailerRequiresInterfacesTag
TrailerReader::SkipToTrailer
SerializedScriptValue::CanDeserializeIn
```

`TrailerOffset` は V8 payload 内の host-object tag としては使用しない。serializer の envelope 書き込みと `TrailerReader` の header 解析でのみ解釈する。
