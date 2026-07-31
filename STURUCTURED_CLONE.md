// scoped_refptr<SerializedScriptValue>
PostMessageHelper::SerializeMessageByMove( // v8::Isolate* isolate, // const
ScriptValue& message, // const StructuredSerializeOptions* options, //
Transferables& transferables, // ExceptionState& exception_state) { // if
(options->hasTransfer() && !options->transfer().empty()) { // if
(!SerializedScriptValue::ExtractTransferables( // isolate, options->transfer(),
transferables, exception_state)) { // return nullptr; // } // }

// SerializedScriptValue::SerializeOptions serialize_options; //
serialize_options.transferables = &transferables; //
scoped_refptr<SerializedScriptValue> serialized_message = //
SerializedScriptValue::Serialize(isolate, message.V8Value(), //
serialize_options, exception_state); // if (exception_state.HadException()) { //
return nullptr; // }

// serialized_message->UnregisterMemoryAllocatedWithCurrentScriptContext(); //
return serialized_message; // }

// bool SerializedScriptValue::ExtractTransferables( // v8::Isolate* isolate, //
const HeapVector<ScriptObject>& object_sequence, // Transferables&
transferables, // ExceptionState& exception_state) { // auto& factory =
SerializedScriptValueFactory::Instance(); // wtf_size_t i = 0; // for (const
auto& script_object : object_sequence) { // // Validation of non-null objects,
per HTML5 spec 10.3.3. // if (script_object.IsNull()) { //
exception_state.ThrowDOMException( // DOMExceptionCode::kDataCloneError, //
StrCat({"Value at index ", String::Number(i), // " is an untransferable 'null'
value."})); // return false; // } // if (!factory.ExtractTransferable(isolate,
script_object.V8Object(), i, // transferables, exception_state)) { // if
(!exception_state.HadException()) { // exception_state.ThrowDOMException( //
DOMExceptionCode::kDataCloneError, // StrCat({"Value at index ",
String::Number(i), // " does not have a transferable type."})); // } // return
false; // } // i++; // } // return true; // } // //
https://source.chromium.org/chromium/chromium/src/+/main:third_party/blink/renderer/bindings/core/v8/serialization/v8_script_value_serializer.cc;drc=60df9044934a2841875904f21e9673afe970da1e;l=90

//
https://source.chromium.org/chromium/chromium/src/+/main:third_party/blink/renderer/core/frame/universal_global_scope.cc;l=70

// if (!script_state->ContextIsValid()) { // return ScriptValue(); // } //
ScriptState::Scope scope(script_state); // v8::Isolate* isolate =
script_state->GetIsolate();

// Transferables transferables; // scoped_refptr<SerializedScriptValue>
serialized_message = // PostMessageHelper::SerializeMessageByMove(isolate,
message, options, // transferables, exception_state);

// if (exception_state.HadException()) { // return ScriptValue(); // }

// DCHECK(serialized_message);

// auto ports = MessagePort::DisentanglePorts( //
ExecutionContext::From(script_state), transferables.message_ports, //
exception_state); // if (exception_state.HadException()) { // return
ScriptValue(); // }

// UnpackedSerializedScriptValue* unpacked = //
SerializedScriptValue::Unpack(std::move(serialized_message)); //
DCHECK(unpacked);

// SerializedScriptValue::DeserializeOptions deserialize_options; // auto
message_ports = MessagePortArray(*MessagePort::EntanglePorts( //
*ExecutionContext::From(script_state), std::move(ports))); //
deserialize_options.message_ports = &message_ports;

// return ScriptValue(isolate, // unpacked->Deserialize(isolate,
deserialize_options));

バイナリ区別のためのタグ実装 //
https://source.chromium.org/chromium/chromium/src/+/main:third_party/blink/renderer/bindings/core/v8/serialization/v8_script_value_serializer.cc
//
https://source.chromium.org/chromium/chromium/src/+/main:third_party/blink/renderer/bindings/core/v8/serialization/v8_script_value_deserializer.cc
//
https://source.chromium.org/chromium/chromium/src/+/main:third_party/blink/renderer/bindings/core/v8/serialization/serialization_tag.h

> 調査の結果、この案は現構造では妥当ではないと判断しました。前の「Blinkはdescriptorから直接codecを選ぶ」という説明は不正確でした。

Blinkの実際の処理は次の構造です。

1. `IsHostObject()`は`IsApiWrapper()`だけを確認
2. `TypeDispatcher`は`WrapperTypeInfo*`を一度取得
3. `WriteDOMObject()`が`ToMostDerived<Blob>()`、`ToMostDerived<File>()`などを順番に試す

つまりBlinkもwrite側は最悪O(n)で、descriptorからcodecへの直接jumpではありません。

Denoで直接jumpを実装するには、全CppGC object生成時にglobal
registryを検索してcodec
pointerをwrapperへ埋め込む必要があります。これは以下の問題があります。

- 全CppGC生成にHashMap lookupが追加される
- coreのwrapperが`ext/web`のstructured clone事情を持つ
- 複数registryで同じ型を異なる形式にserializeできなくなる
- wrapper layoutとstructured clone実装が密結合になる
- `#[webidl(serializable)]`だけではgenericなCppGC生成処理を特殊化できない

そのためdescriptor実装は残していません。現在の「CppGC `TypeId`取得 + registry
HashMap」の方が責務分離を維持でき、期待O(1)なので、Blinkの線形dispatchよりも型数増加に強い構造です。
