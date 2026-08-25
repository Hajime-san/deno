# kTrailerOffsetTagについて

https://source.chromium.org/search?q=kTrailerOffsetTag&ss=chromium%2Fchromium%2Fsrc

**Blinkでの用途** Blink の wire format は概ね次の構造です。

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

serialization 開始時には offset と size をゼロで予約します。host object
の書き込み中、`WriteAndRequireInterfaceTag()` が使用した Web IDL interface tag
を `TrailerWriter` に記録します。

最後に次の trailer を末尾へ追加します。

```text
A0
interface_count: u32 big-endian
interface_tags: [u8; interface_count]
```

その後、予約していた `trailer_offset` と `trailer_size`
を実値で上書きします。trailer がなければ両方ともゼロです。

**Deserializeでの用途** `TrailerReader::SkipToTrailer()` が offset を使って V8
payload を解析せずに trailer へ直接移動します。

`SerializedScriptValue::CanDeserializeIn()` は trailer に記録された interface
tag を調べ、受信 realm がすべての interface を公開しているか確認します。

つまり目的は次です。

> 実際に object graph を deserialize する前に、受信 realm で必要な platform
> interface が利用可能か判定する。

`ImageData`、`Blob`、`MessagePort`、各種 Stream などが
`WriteAndRequireInterfaceTag()` を使用しています。

**Denoでのexposure検査の現状** Deno では platform object 共通の
`StructuredCloneHostObject` trait に、次の API を追加済みである。

```rust
fn is_exposed<'s, 'i>(
  scope: &mut v8::PinScope<'s, 'i>,
  target_realm: v8::Local<'s, v8::Context>,
) -> bool;
```

registry は interface ごとの handler
としてこの関数を保持する。現在は次のタイミングで使用している。

- 通常の deserialize では、trailer に記録された全 interface tag を V8 payload
  の処理前に検査する。加えて、V8 の再帰処理が serialized host-object tag
  に到達した時点でも検査する。これは StructuredDeserialize の step 22.2
  に対応する。
- transfer 付き deserialize では、transfer data から受信側 object
  を生成する前に検査する。これは StructuredDeserializeWithTransfer の step 4.2
  に対応する。

`ImageData::is_exposed()` は現在 `true` を返す。Deno の `ImageData` は Window と
Worker の両方に公開されるためである。

serializer は host object の書き込みに成功した時点で interface tag を
`TrailerWriter` に記録する。serialization 終了後、重複を除いた tag を trailer
として追加し、buffer 先頭の offset/size を patch する。

trailer の事前検査と transfer data の受信経路は、非 exposed 時に
`DOMExceptionDataCloneError` の `JsErrorBox` を返す。trailer に必要な tag
がない不正な data を callback 内で検出した場合の fallback は、現在も plain V8
`Error` を throw する。

**Denoでの採用方針** 現行の `structuredClone` は Deno 独自の外側
envelope を持たず、V8 の `ValueSerializer::WriteHeader()` が生成する次の形式を
同一 op 内で直ちに deserialize している。

```text
V8 header (FF | V8 version) | V8 payload
```

新しい structured-clone serializer では、この V8 data の外側に Blink と同じ
構造の Deno-controlled envelope を導入する。

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

`SerializationTag` は host-object tag だけではなく、structured-clone wire format
で Deno が解釈する tag の共通名前空間とする。このため
`ImageData`、`Version`、`TrailerOffset` を同じ enum に含めることは Blink の
`SerializationTag` と同じ設計である。各 variant が使用できる位置は wire format
の文法によって決まる。

`TrailerOffset` に続く offset と size の領域は常に確保する。必要な interface
がなければ両方をゼロにする。trailer があれば、offset は buffer 先頭から trailer
先頭までの byte 数、size は trailer の byte 数とし、どちらも big-endian
で記録する。

現在の実装は以下を含む。

- host object の書き込み時に使用した interface tag を収集する `TrailerWriter`
- trailer の追加後に offset/size を big-endian で patch する処理
- offset/size の範囲を検証する `TrailerReader`
- `SerializationTag::TrailerRequiresInterfaces` (`A0`) の読み書き
- trailer の interface tag から対応する `is_exposed` handler を引く registry API
- deserialize 前に必要な interface を検査する処理
- 非 exposed 時に `DataCloneError` DOMException を返す処理
- 新形式に合わせた wire-format fixture

現行 `structuredClone` が生成する V8-only data は外部へ返されず、その呼び出し中
に消費される一時 data である。この経路を新しい serializer/deserializer の組に
置き換える場合、V8-only 形式との並行サポートは不要である。

一方、新しい deserializer に過去の `core.serialize` の出力も受け入れさせる場合は、
その出力も同じ V8-only 形式なので、別途 legacy reader が必要になる。新旧とも先頭
が `FF | version` で始まるため、新形式では Deno version の直後に続く `FE` の有無で
識別することになる。この永続データ互換性は `structuredClone` の一時 data の互換性
とは別の要件として扱う。

検索は `kTrailerOffsetTag` から始め、次の順で追うと全体を把握できます。

```text
kTrailerOffsetTag
TrailerWriter
WriteAndRequireInterfaceTag
kTrailerRequiresInterfacesTag
TrailerReader::SkipToTrailer
SerializedScriptValue::CanDeserializeIn
```

`TrailerOffset` は V8 payload 内の host-object tag
としては使用しない。serializer の envelope 書き込みと `TrailerReader` の header
解析でのみ解釈する。
