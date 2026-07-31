# Integrating Rust Transferables with `postMessage`

## Scope

The HTML `postMessage` algorithms use `StructuredSerializeWithTransfer` and
`StructuredDeserializeWithTransfer`. The Rust structured-clone backend should
therefore eventually be shared by `structuredClone`, workers, and message ports
rather than maintaining a separate definition of Rust host-object transfer.

This document describes the work required to use
`structured_serialize_with_transfer` and `structured_deserialize_with_transfer`
for cross-isolate `postMessage`. It is an integration plan, not a description of
functionality that is already connected to workers.

Specification references:

- https://html.spec.whatwg.org/multipage/web-messaging.html#posting-messages
- https://html.spec.whatwg.org/multipage/structured-data.html#structuredserializewithtransfer
- https://html.spec.whatwg.org/multipage/structured-data.html#structureddeserializewithtransfer

## Current `postMessage` Path

Worker and `DedicatedWorkerGlobalScope` entry points currently convert
`StructuredSerializeOptions` in JavaScript and call `serializeJsMessageData`
from `ext/web/13_message_port.js`.

That implementation is independent from the Rust structured-clone backend:

1. It divides the transfer list into `ArrayBuffer`s and objects branded with
   `core.hostObjectBrand`.
2. It calls `core.serialize` with V8 host objects and transferred array buffers.
3. It transfers branded objects through
   `core.getTransferableResource(type).send(object)`.
4. It sends a `JsMessageData` containing an owned `DetachedBuffer` and a list of
   `JsTransferable` records.
5. The receiving isolate reconstructs resources and calls `core.deserialize`.

The Rust transport representation in `ext/web/message_port.rs` is currently:

```rust
pub enum Transferable {
  Resource(String, Box<dyn TransferredResource>),
  MultiResource(String, Vec<Box<dyn TransferredResource>>),
  ArrayBuffer(u32),
}
```

`TransferredResource` is `Send`, allowing this value to cross a worker thread.
This path already supports `MessagePort`, streams, and Node worker resources; an
integration must not regress them.

The no-transfer-list path uses raw-buffer operations to avoid allocating and
converting `JsMessageData`. It is a hot path and should remain unchanged.

## Existing Rust WithTransfer Backend

The Rust backend already provides the algorithmic pieces needed for transfer:

- an owned `Vec<u8>` serialized representation with no source-isolate handles;
- transfer-list duplicate detection and validation;
- `ArrayBuffer` detachment and backing-store transfer;
- host-object validation, transfer, and receive hooks;
- type-specific transfer-reference tags in the V8 payload;
- preservation of object aliases through V8 serialization;
- reconstruction of the ordered `[[TransferredValues]]` list.

The serialized representation is therefore already suitable for persistence or
cross-isolate transport. The remaining work is making host-object transfer
holders Send-capable and connecting them to the worker transport and the
destination registry.

## Serialized Representation

`StructuredSerializeWithTransferResult` now contains owned serialized bytes; the
intermediate structured-clone layer deliberately does not return a
source-isolate `v8::Local` fast path for primitives:

```rust
struct StructuredSerializeWithTransferResult<T> {
  serialized: Vec<u8>,
  transfer_data_holders: Vec<StructuredCloneTransferData<T>>,
}
```

The public `structuredClone` entry point returns cloneable primitives directly
after converting its options, but only when the transfer list is empty. The
intermediate serializer always produces bytes so its result can be persisted or
moved to another isolate. A primitive value with a non-empty transfer list must
still run WithTransfer so unreachable list entries are transferred and detached
as required by the specification.

This removes the need for a separate postMessage-only serialized value type. The
same bytes can be passed to same-isolate deserialization, persisted by a storage
API, or sent to another isolate.

## Remaining Cross-Isolate Transfer Data Work

Transfer data crossing a worker channel must be `Send + 'static`. This requires
either:

```rust
type TransferData: Send + 'static;
```

or a separate cross-isolate API whose registry transfer data has that bound.
Keeping the general same-isolate API less restrictive is possible, but every
type exposed through `postMessage` must satisfy the stronger bound.

The current Web registry erases transfer data as `Box<dyn Any>`. Its
cross-isolate form must use `Box<dyn Any + Send>` or another explicitly
Send-capable holder.

## Message Transport Changes

The MessagePort transport must be able to carry Rust host-object transfer data
out of band. There are two viable designs:

1. Add a Rust host-object holder variant to `Transferable`.
2. Adapt each holder to the existing `TransferredResource` abstraction.

A dedicated variant makes the structured-clone responsibility clearer, while
`TransferredResource` reuses the established Send-capable transport. The final
choice must preserve the ordered relationship between transfer-list entries,
transfer-reference indexes in the serialized payload, and received values.

Transfer data holders are not persisted wire data. They live only for the
delivery of one message. The serialized payload still contains stable,
type-specific transfer-reference tags and indexes. Changes to the meaning of
those persisted tags remain subject to the Deno structured-clone wire-version
rules.

## Registry Composition

The destination isolate must have a receive handler for every transferred host
object before deserialization starts. The registry used by workers therefore has
to compose registrations supplied by extensions such as `deno_web`,
`deno_image`, and `deno_canvas`.

Registration must retain the existing separation of identities:

- Web IDL interface names select runtime handlers.
- Fixed structured-clone tags identify records in serialized bytes.
- Neither persistent data nor Web API behavior should depend on Rust `TypeId`.

The sending registry validates and creates transfer data. The receiving registry
consumes that data and constructs a new wrapper in the target isolate. A missing
receiving registration must produce a `DataCloneError`, not silently deserialize
an ordinary object.

## Proposed Send Sequence

For a message with a non-empty transfer list:

1. Perform Web IDL conversion of `StructuredSerializeOptions`.
2. Validate the complete transfer list, including duplicates and detached
   objects, before changing any source object.
3. Serialize the complete graph with transfer-reference indexes.
4. If serialization fails, leave all source objects attached.
5. Detach or transfer each prepared object and produce Send-capable holders.
6. Send the already-owned bytes and holders through the worker or MessagePort
   channel.

Serialization and transfer should occur in one synchronous Rust operation if the
Rust backend owns this path. Passing already-erased `JsMessageData` into the
current posting op is too late because the V8 values and transfer-list
identities are no longer available there.

## Proposed Receive Sequence

In the destination isolate:

1. Resolve each Rust transfer holder through the destination registry.
2. Reconstruct transferred `ArrayBuffer`s and host-object wrappers in transfer
   list order.
3. Supply those objects to `structured_deserialize_with_transfer`.
4. Return the deserialized value and the ordered transferred-values list
   required by the specification.
5. Dispatch the resulting `MessageEvent` using the existing MessagePort or
   worker event path.

Receive failure must not attempt to reuse a consumed transfer holder. Error
behavior and channel cleanup therefore need explicit tests.

## Incremental Migration

The lowest-risk migration is:

1. Keep the no-transfer raw-buffer fast path unchanged.
2. Reuse the existing owned `Vec<u8>` result for the message payload.
3. Make the existing host-object transfer holders Send-capable for worker
   transport.
4. Extend MessagePort transport with Rust host-object holders.
5. Compose destination registries from participating extensions.
6. Route only Rust host-object transfers through the new path initially while
   preserving existing resource transfer for MessagePort and streams.
7. Migrate or unify the remaining legacy resource types only after behavioral
   parity is established.

The new `"DENO" | version | V8 payload` envelope differs from the current
`core.serialize` output. Switching worker serialization therefore needs
compatibility testing for all existing cloneable and transferable values; it
must not be treated as an ImageBitmap-only change.

## Object-Specific Feasibility

### `ImageBitmap`

`ImageBitmap` is the best first Rust host-object candidate. Its detached state
and image data can conceptually be moved into a Send-capable transfer holder.
The implementation must verify that the concrete image representation is Send,
perform detachment only after successful serialization, and reconstruct a fresh
CppGC wrapper in the receiving isolate.

`ImageBitmap` is both `[Serializable]` and `[Transferable]`. Cloning without a
transfer entry and transferring with an entry must use separate wire tags and
separate code paths, following Blink's model.

### `OffscreenCanvas`

`OffscreenCanvas` is not currently ready for cross-worker Rust transfer. It
contains `Rc<RefCell<DynamicImage>>` and an active context stored as a
`v8::Global`. Neither can be moved directly to another isolate.

Supporting it requires separating transferable canvas state from isolate-local
wrapper/context state. The receiving isolate must create a new context-facing
wrapper rather than transporting a V8 handle. Same-isolate structured-clone
experiments must not be presented as complete worker transfer support.

## Required Tests

Integration is complete only when tests cover:

- `ImageBitmap` transfer from parent to worker and worker to parent;
- detachment after successful posting;
- no detachment after validation or serialization failure;
- duplicate transfer-list entries;
- transferred objects reachable multiple times in the serialized graph;
- transfer-list objects not reachable from the serialized value;
- the ordering of mixed `ArrayBuffer`, legacy resource, and Rust host-object
  transfers;
- missing destination registry handlers;
- receive failures and holder cleanup;
- MessagePort and stream transfer regressions;
- the no-transfer raw-buffer fast path;
- structured-clone wire fixtures for transfer-reference tags where applicable.

WPT coverage should be used where available, but Deno-specific cross-thread
transport and fixed-wire-format behavior also require Rust or integration tests.

## Completion Criteria

Rust-defined transferables are available to `postMessage` only when all of the
following are true:

- serialized data contains no source-isolate V8 handles (implemented);
- every cross-thread transfer holder is Send-capable;
- the destination registry can receive every holder type;
- validation happens before detachment;
- legacy transferable resources continue to work;
- transfer ordering and aliasing match the HTML algorithms;
- the no-transfer fast path retains its current behavior and performance.
