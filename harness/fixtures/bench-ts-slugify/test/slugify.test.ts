import { test } from "node:test";
import assert from "node:assert/strict";
import { slugify } from "../src/slugify.ts";

test("lowercases and joins words", () => {
  assert.equal(slugify("Hello World"), "hello-world");
});

test("collapses runs of punctuation and spaces", () => {
  assert.equal(slugify("Rust,  Go & TypeScript!"), "rust-go-typescript");
});

test("trims leading and trailing separators", () => {
  assert.equal(slugify("  --Tandem IDE--  "), "tandem-ide");
});

test("strips accents", () => {
  assert.equal(slugify("Crème Brûlée"), "creme-brulee");
});
