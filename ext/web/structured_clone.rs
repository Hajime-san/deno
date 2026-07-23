// Copyright 2018-2026 the Deno authors. MIT license.

use deno_core::v8;
use deno_core::op2;

use deno_error::JsErrorBox;

// scoped_refptr<SerializedScriptValue> PostMessageHelper::SerializeMessageByMove(
//     v8::Isolate* isolate,
//     const ScriptValue& message,
//     const StructuredSerializeOptions* options,
//     Transferables& transferables,
//     ExceptionState& exception_state) {
//   if (options->hasTransfer() && !options->transfer().empty()) {
//     if (!SerializedScriptValue::ExtractTransferables(
//             isolate, options->transfer(), transferables, exception_state)) {
//       return nullptr;
//     }
//   }

//   SerializedScriptValue::SerializeOptions serialize_options;
//   serialize_options.transferables = &transferables;
//   scoped_refptr<SerializedScriptValue> serialized_message =
//       SerializedScriptValue::Serialize(isolate, message.V8Value(),
//                                        serialize_options, exception_state);
//   if (exception_state.HadException()) {
//     return nullptr;
//   }

//   serialized_message->UnregisterMemoryAllocatedWithCurrentScriptContext();
//   return serialized_message;
// }

// bool SerializedScriptValue::ExtractTransferables(
//     v8::Isolate* isolate,
//     const HeapVector<ScriptObject>& object_sequence,
//     Transferables& transferables,
//     ExceptionState& exception_state) {
//   auto& factory = SerializedScriptValueFactory::Instance();
//   wtf_size_t i = 0;
//   for (const auto& script_object : object_sequence) {
//     // Validation of non-null objects, per HTML5 spec 10.3.3.
//     if (script_object.IsNull()) {
//       exception_state.ThrowDOMException(
//           DOMExceptionCode::kDataCloneError,
//           StrCat({"Value at index ", String::Number(i),
//                   " is an untransferable 'null' value."}));
//       return false;
//     }
//     if (!factory.ExtractTransferable(isolate, script_object.V8Object(), i,
//                                      transferables, exception_state)) {
//       if (!exception_state.HadException()) {
//         exception_state.ThrowDOMException(
//             DOMExceptionCode::kDataCloneError,
//             StrCat({"Value at index ", String::Number(i),
//                     " does not have a transferable type."}));
//       }
//       return false;
//     }
//     i++;
//   }
//   return true;
// }
//
// https://source.chromium.org/chromium/chromium/src/+/main:third_party/blink/renderer/bindings/core/v8/serialization/v8_script_value_serializer.cc;drc=60df9044934a2841875904f21e9673afe970da1e;l=90

// https://source.chromium.org/chromium/chromium/src/+/main:third_party/blink/renderer/core/frame/universal_global_scope.cc;l=70

#[op2]
pub fn structured_clone<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  value: v8::Local<'a, v8::Value>,
  // #[varargs] options: Option<&v8::Value>,
  // options: const StructuredSerializeOptions* options,
) -> Result<v8::Global<v8::Value>, JsErrorBox> {
  // if (!script_state->ContextIsValid()) {
  //   return ScriptValue();
  // }
  // ScriptState::Scope scope(script_state);
  // v8::Isolate* isolate = script_state->GetIsolate();

  // Transferables transferables;
  // scoped_refptr<SerializedScriptValue> serialized_message =
  //     PostMessageHelper::SerializeMessageByMove(isolate, message, options,
  //                                               transferables, exception_state);

  // if (exception_state.HadException()) {
  //   return ScriptValue();
  // }

  // DCHECK(serialized_message);

  // auto ports = MessagePort::DisentanglePorts(
  //     ExecutionContext::From(script_state), transferables.message_ports,
  //     exception_state);
  // if (exception_state.HadException()) {
  //   return ScriptValue();
  // }

  // UnpackedSerializedScriptValue* unpacked =
  //     SerializedScriptValue::Unpack(std::move(serialized_message));
  // DCHECK(unpacked);

  // SerializedScriptValue::DeserializeOptions deserialize_options;
  // auto message_ports = MessagePortArray(*MessagePort::EntanglePorts(
  //     *ExecutionContext::From(script_state), std::move(ports)));
  // deserialize_options.message_ports = &message_ports;

  // return ScriptValue(isolate,
  //                    unpacked->Deserialize(isolate, deserialize_options));


  return Err(JsErrorBox::new(
    "Error",
    "👺👺👺",
  ));

  let context = scope.get_current_context();
  v8::tc_scope!(tc_scope, scope);

  if tc_scope.has_caught() {
    return Ok(v8::Global::new(tc_scope, value));
  }
  // https://html.spec.whatwg.org/multipage/structured-data.html#dom-structuredclone
  // 1.


  return Ok(v8::Global::new(tc_scope, value));
}

// deno_core::extension!(
//   deno_structured_clone,
//   ops = [structuredClone],
//   state = |_state| {
//   },
// );
