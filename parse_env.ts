import { parseEnv, parseEnvJs } from "node:util";

// https://github.com/denoland/node_test/blob/170b25ab9080b9d58d21b582fbf28ee5676b8387/test/fixtures/dotenv/valid.env
const env = Deno.readTextFileSync("valid.env");

Deno.bench("parseEnv", { group: "parseEnv", baseline: true }, () => {
  parseEnv(env);
});

Deno.bench("parseEnvJs", { group: "parseEnv" }, () => {
  parseEnvJs(env);
});

// % target/release/deno bench --no-check -R parse_env.ts
//     CPU | Apple M4
// Runtime | Deno 2.6.9 (aarch64-apple-darwin)
//
// | benchmark    | time/iter (avg) |        iter/s |      (min … max)      |      p75 |      p99 |     p995 |
// | ------------ | --------------- | ------------- | --------------------- | -------- | -------- | -------- |
//
// group parseEnv
// | parseEnv     |         15.4 µs |        64,780 | ( 13.2 µs … 633.5 µs) |  15.5 µs |  24.2 µs |  28.3 µs |
// | parseEnvJs   |         25.5 µs |        39,150 | ( 22.5 µs … 332.5 µs) |  25.5 µs |  31.8 µs |  34.8 µs |
//
// summary
//   parseEnv
//      1.66x faster than parseEnvJs
