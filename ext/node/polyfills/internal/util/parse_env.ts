// Copyright 2018-2026 the Deno authors. MIT license.
// Copyright Node.js contributors. All rights reserved. MIT License.

import { primordials } from "ext:core/mod.js";
import { validateString } from "ext:deno_node/internal/validators.mjs";

const {
  StringPrototypeCharCodeAt,
  StringPrototypeSlice,
} = primordials;

export function parseEnv(content: string): Record<string, string> {
  validateString(content, "content");
  return parseContent(content);
}

const CHAR_NL = 10; // \n
const CHAR_CR = 13; // \r
const CHAR_TAB = 9; // \t
const CHAR_SPACE = 32; // " "
const CHAR_HASH = 35; // #
const CHAR_EQ = 61; // =
const CHAR_DQUOTE = 34; // "
const CHAR_SQUOTE = 39; // '
const CHAR_BQUOTE = 96; // `

function parseContent(content: string): Record<string, string> {
  const env: Record<string, string> = {};

  let text = removeCarriageReturns(content);
  text = trimSpaces(text);

  while (text.length > 0) {
    // Skip empty lines and comments
    const firstChar = StringPrototypeCharCodeAt(text, 0);
    if (firstChar === CHAR_NL || firstChar === CHAR_HASH) {
      const newline = findChar(text, CHAR_NL, 0);
      if (newline !== -1) {
        text = StringPrototypeSlice(text, newline + 1);
      } else {
        text = "";
      }
      continue;
    }

    const equalOrNewline = findEqOrNewline(text);

    // If no equals or newline before equals, skip invalid line
    if (
      equalOrNewline === -1 ||
      StringPrototypeCharCodeAt(text, equalOrNewline) === CHAR_NL
    ) {
      if (equalOrNewline !== -1) {
        text = trimSpaces(StringPrototypeSlice(text, equalOrNewline + 1));
        continue;
      }
      break;
    }

    let key = trimSpaces(StringPrototypeSlice(text, 0, equalOrNewline));
    text = StringPrototypeSlice(text, equalOrNewline + 1);

    // If the value is not present (e.g. KEY=) set it to an empty string
    if (text.length === 0 || StringPrototypeCharCodeAt(text, 0) === CHAR_NL) {
      env[key] = "";
      continue;
    }

    text = trimSpaces(text);

    // Skip lines with empty keys after trimming spaces
    if (key.length === 0) {
      continue;
    }

    // Remove export prefix from key and ensure proper spacing
    if (startsWithExport(key)) {
      key = trimSpaces(StringPrototypeSlice(key, 7));
    }

    if (text.length === 0) {
      env[key] = "";
      break;
    }

    // Expand new line if \n it's inside double quotes
    if (StringPrototypeCharCodeAt(text, 0) === CHAR_DQUOTE) {
      const closingQuote = findChar(text, CHAR_DQUOTE, 1);
      if (closingQuote !== -1) {
        let value = StringPrototypeSlice(text, 1, closingQuote);
        value = replaceEscapedNewlines(value);
        env[key] = value;

        const newline = findChar(text, CHAR_NL, closingQuote + 1);
        if (newline !== -1) {
          text = StringPrototypeSlice(text, newline + 1);
        } else {
          text = "";
        }
        continue;
      }
    }

    // Handle quoted values (single quotes, double quotes, backticks)
    const quote = StringPrototypeCharCodeAt(text, 0);
    if (
      quote === CHAR_SQUOTE || quote === CHAR_DQUOTE || quote === CHAR_BQUOTE
    ) {
      const closingQuote = findChar(text, quote, 1);

      if (closingQuote === -1) {
        const newline = findChar(text, CHAR_NL, 0);
        if (newline !== -1) {
          const value = StringPrototypeSlice(text, 0, newline);
          env[key] = value;
          text = StringPrototypeSlice(text, newline + 1);
        } else {
          env[key] = text;
          break;
        }
      } else {
        const value = StringPrototypeSlice(text, 1, closingQuote);
        env[key] = value;
        const newline = findChar(text, CHAR_NL, closingQuote + 1);
        if (newline !== -1) {
          text = StringPrototypeSlice(text, newline + 1);
        } else {
          text = "";
        }
        continue;
      }
    } else {
      // Regular key value pair
      const newline = findChar(text, CHAR_NL, 0);
      if (newline !== -1) {
        let value = StringPrototypeSlice(text, 0, newline);
        const hash = findChar(value, CHAR_HASH, 0);
        if (hash !== -1) {
          value = StringPrototypeSlice(value, 0, hash);
        }
        value = trimSpaces(value);
        env[key] = value;
        text = StringPrototypeSlice(text, newline + 1);
      } else {
        let value = text;
        const hash = findChar(value, CHAR_HASH, 0);
        if (hash !== -1) {
          value = StringPrototypeSlice(value, 0, hash);
        }
        env[key] = trimSpaces(value);
        text = "";
      }
    }

    text = trimSpaces(text);
  }

  return env;
}

function trimSpaces(input: string): string {
  if (input.length === 0) return "";
  let start = 0;
  let end = input.length - 1;

  while (start <= end) {
    const c = StringPrototypeCharCodeAt(input, start);
    if (c !== CHAR_SPACE && c !== CHAR_TAB && c !== CHAR_NL) break;
    start++;
  }

  while (end >= start) {
    const c = StringPrototypeCharCodeAt(input, end);
    if (c !== CHAR_SPACE && c !== CHAR_TAB && c !== CHAR_NL) break;
    end--;
  }

  if (end < start) return "";
  return StringPrototypeSlice(input, start, end + 1);
}

function removeCarriageReturns(input: string): string {
  let out = "";
  let i = 0;
  while (i < input.length) {
    const c = StringPrototypeCharCodeAt(input, i);
    if (c !== CHAR_CR) {
      out += StringPrototypeSlice(input, i, i + 1);
    }
    i++;
  }
  return out;
}

function replaceEscapedNewlines(input: string): string {
  let out = "";
  let i = 0;
  while (i < input.length) {
    const c = StringPrototypeCharCodeAt(input, i);
    if (c === 92 /* backslash */ && i + 1 < input.length) {
      const next = StringPrototypeCharCodeAt(input, i + 1);
      if (next === 110 /* n */) {
        out += "\n";
        i += 2;
        continue;
      }
    }
    out += StringPrototypeSlice(input, i, i + 1);
    i++;
  }
  return out;
}

function startsWithExport(input: string): boolean {
  if (input.length < 7) return false;
  return (
    StringPrototypeCharCodeAt(input, 0) === 101 && // e
    StringPrototypeCharCodeAt(input, 1) === 120 && // x
    StringPrototypeCharCodeAt(input, 2) === 112 && // p
    StringPrototypeCharCodeAt(input, 3) === 111 && // o
    StringPrototypeCharCodeAt(input, 4) === 114 && // r
    StringPrototypeCharCodeAt(input, 5) === 116 && // t
    StringPrototypeCharCodeAt(input, 6) === CHAR_SPACE // space
  );
}

function findChar(input: string, charCode: number, from: number): number {
  let i = from;
  while (i < input.length) {
    if (StringPrototypeCharCodeAt(input, i) === charCode) return i;
    i++;
  }
  return -1;
}

function findEqOrNewline(input: string): number {
  let i = 0;
  while (i < input.length) {
    const c = StringPrototypeCharCodeAt(input, i);
    if (c === CHAR_EQ || c === CHAR_NL) return i;
    i++;
  }
  return -1;
}
