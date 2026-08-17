# Serialization

This directory implements Deno's structured-clone envelope around V8's value
serializer. Data produced here can be persisted by embedders, so its wire format
is a compatibility contract.

The envelope is `0xFE | Deno version:uint32(varint) | V8 header | V8 payload`.
The `0xFE` marker is consumed by Deno before constructing the V8 deserializer.

## Version boundaries

There are three independent identifiers:

| Identifier                                  | Owner | Purpose                                                                      |
| ------------------------------------------- | ----- | ---------------------------------------------------------------------------- |
| `WIRE_FORMAT_VERSION`      | Deno  | The `0xFE` envelope, host-object tags, and Deno host-object payload schemas. |
| V8 serializer header version                | V8    | The format for ECMAScript built-ins and nested V8 values.                    |
| Host-object registry tag                    | Deno  | A permanent identifier for one host-object representation.                   |

V8's serializer format is implemented by V8; see
[`value-serializer.cc`](https://source.chromium.org/chromium/chromium/src/+/main:v8/src/objects/value-serializer.cc).
Host-object semantics are embedder code. Blink's structured-clone implementation
is a useful reference, but Deno does not share Blink's host-object wire format.

Never renumber, reuse, or repurpose a host-object registry tag. Retired values
remain reserved.

## Changing a Deno host-object payload

For example, `ImageData` writes a Deno-defined payload after its registered
`ImageData` tag byte. Increment the Deno wire version when changing an existing
tag's payload would make a current decoder interpret a valid older payload
differently. Then do all of the following:

1. Increment `WIRE_FORMAT_VERSION`.
2. Add a decoder branch for the previous version in the host object's
   `read_structured_clone_payload` implementation.
3. Keep every existing fixture and its decode test.
4. Add a fixture and encode test for the new current format in
   `ext/web/structured_clone/tests/wire_format_backward_compatibility.rs`.

The fixture name should include both the Deno envelope version and V8 header
version, for example `IMAGE_DATA_V2_V8_16`. An encode test asserts that the
current writer produces the current fixture. Decode tests assert that the
current reader continues to accept every released fixture.

Do not edit or delete a fixture after its format has shipped. Before a format
has shipped, its fixture may be updated without incrementing the version.

Optional host-object fields should use a self-describing encoding with defaults
for absent fields, as `ImageData` does with its settings subtags. This lets new
runtimes read older payloads. Adding a new host-object tag, or appending a new
self-describing optional field, does not by itself require a Deno wire-version
bump: no valid older payload contains that new tag. Older runtimes need not read
values newly written with it. Unknown subtags must not be silently accepted
unless their full encoded shape can be safely skipped.

## Updating V8

When only V8's serializer header version changes, leave
`WIRE_FORMAT_VERSION` unchanged. Add a new current encoder
fixture whose name includes the new V8 version, and retain older fixtures as
decode tests. Bump the Deno envelope version only when Deno-controlled bytes
(need versioned decoding to preserve the interpretation of older payloads).
