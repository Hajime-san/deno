# Structured Clone Failures

## WPT runner

Command:

```sh
mise exec -- target/debug/deno run -A --config tools/deno.json \
  tests/wpt/wpt.ts run --binary=target/debug/deno -- \
  html/webappapis/structured-clone
```

The WPT manifest was generated with mise's Python 3.11.15. The runner requires
`/etc/hosts` entries for `web-platform.test`, `not-web-platform.test`, and their
subdomains:

```sh
mise exec -- sh -c 'cd tests/wpt/suite && python3 ./wpt make-hosts-file' \
  | sudo tee -a /etc/hosts
```

Result: 2 failed files, 1 expected failure, 1 passed. Both
`structured-clone.any.html` and `structured-clone.any.worker.html` had 33 failed
subtests (101 passed, 3 existing expected failures).

| Failure group               | Failed subtests in each any/worker file                                                                                                                                        |
| --------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Blob                        | 19 cases: standalone, array, and object-nested Blob values no longer deserialize as Blob.                                                                                      |
| File                        | `File basic` and subclass deserialization return a plain object.                                                                                                               |
| MessagePort / transferables | `MessagePort`, detached object transfer, deleted global interface, transferable subclass, and ReadableStream transfer fail.                                                    |
| Serialization errors        | Throwing getter and out-of-bounds TypedArray/DataView cases return `RangeError: Cannot deserialize structured clone magic` rather than the original error or `DataCloneError`. |
| Platform-object policy      | Non-serializable platform object rejection and deleted-global-interface deserialization fail.                                                                                  |

The expected failure `Transferring a non-transferable platform object fails` now
passes, so the expectations runner also reports it as an unexpected pass.

An earlier WPT run used a stale `target/debug/deno` binary and panicked while
formatting an error. After rebuilding Deno with the current sources, that panic
did not reproduce; the failures above are the relevant results.

## Unit tests

Command:

```sh
target/debug/deno test --quiet --config tests/config/deno.json \
  --allow-all --location=http://js-unit-tests/foo/bar \
  tests/unit/structured_clone_test.ts
```

Result: 4 failures, 1 pass.

| Test                                        | Failure                                                                                               |
| ------------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| `self.structuredClone`                      | `MessagePort` in a transfer list throws `DataCloneError: Value in transfer list is not transferable`. |
| `correct DataCloneError message`            | Expected `Value not transferable`; received `Value in transfer list is not transferable`.             |
| `structuredClone URL throws DataCloneError` | `structuredClone(new URL(...))` does not throw.                                                       |
| `structuredClone CryptoKey`                 | A cloned `CryptoKey` loses its fields; `type` is `undefined` instead of `secret`.                     |

The active ext/web Rust fallback handles the Web registry but does not include
the core serializer's MessagePort transfer state, JS uncloneable markers, or
`hostObjectBrand`/`cloneableDeserializers` protocol. These failures must be
resolved before this path can replace the generic core structured-clone path.
