import { describe, expect, it } from "vitest";
import {
  MAXIMUM_SAFE_WIRE_INTEGER,
  WireError,
  decodeWireDate,
  encodeWireDate,
  isValidWireIdentifier,
  requireWireInteger,
  secondsToWireMilliseconds,
} from "../../src/companion/protocol/wire.js";

describe("wire dates", () => {
  it("encodes epoch ms as RFC3339 with exactly 3 fractional digits + Z", () => {
    expect(encodeWireDate(1_753_500_012_345)).toMatch(
      /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/,
    );
    expect(encodeWireDate(0)).toBe("1970-01-01T00:00:00.000Z");
  });

  it("round-trips encode -> decode", () => {
    const ms = 1_753_500_012_345;
    expect(decodeWireDate(encodeWireDate(ms))).toBe(ms);
  });

  const rejected = [
    "2026-07-26T09:41:12Z", // no milliseconds
    "2026-07-26T09:41:12.34Z", // 2 digits
    "2026-07-26T09:41:12.345678Z", // 6 digits
    "2026-07-26T09:41:12.345+00:00", // offset form
    "2026-07-26 09:41:12.345Z", // space separator
    "2026-13-26T09:41:12.345Z", // invalid month
  ];
  for (const value of rejected) {
    it(`rejects non-canonical date ${JSON.stringify(value)}`, () => {
      expect(() => decodeWireDate(value)).toThrow(WireError);
    });
  }
});

describe("wire identifiers", () => {
  it("accepts UUIDs (any case) and Crockford ULIDs", () => {
    expect(isValidWireIdentifier("3f6f6c0a-58a8-4a9d-b0a8-1c2d3e4f5a6b")).toBe(true);
    expect(isValidWireIdentifier("3F6F6C0A-58A8-4A9D-B0A8-1C2D3E4F5A6B")).toBe(true);
    expect(isValidWireIdentifier("01ARZ3NDEKTSV4RRFFQ69G5FAV")).toBe(true);
  });
  it("rejects ULIDs containing I/L/O/U, lowercase, wrong length", () => {
    expect(isValidWireIdentifier("01ARZ3NDEKTSV4RRFFQ69G5FAI")).toBe(false);
    expect(isValidWireIdentifier("01arz3ndektsv4rrffq69g5fav")).toBe(false);
    expect(isValidWireIdentifier("01ARZ3NDEKTSV4RRFFQ69G5FA")).toBe(false);
    expect(isValidWireIdentifier("not-an-id")).toBe(false);
  });
});

describe("wire integers", () => {
  it("accepts 0..2^53-1", () => {
    expect(requireWireInteger(0, "x")).toBe(0);
    expect(requireWireInteger(MAXIMUM_SAFE_WIRE_INTEGER, "x")).toBe(
      MAXIMUM_SAFE_WIRE_INTEGER,
    );
  });
  it("rejects negatives, floats, and beyond-safe values", () => {
    expect(() => requireWireInteger(-1, "x")).toThrow(WireError);
    expect(() => requireWireInteger(1.5, "x")).toThrow(WireError);
    expect(() => requireWireInteger(MAXIMUM_SAFE_WIRE_INTEGER + 1, "x")).toThrow(
      WireError,
    );
  });
});

describe("secondsToWireMilliseconds", () => {
  it("rounds to integer milliseconds", () => {
    expect(secondsToWireMilliseconds(1.2345, "x")).toBe(1235);
    expect(secondsToWireMilliseconds(0, "x")).toBe(0);
  });
  it("rejects negative and non-finite", () => {
    expect(() => secondsToWireMilliseconds(-0.001, "x")).toThrow(WireError);
    expect(() => secondsToWireMilliseconds(Number.NaN, "x")).toThrow(WireError);
    expect(() => secondsToWireMilliseconds(Number.POSITIVE_INFINITY, "x")).toThrow(
      WireError,
    );
  });
});
