const test = require("node:test");
const assert = require("node:assert");
const { parseSignup, SignupSchema } = require("../user.js");

const ok = { email: "Ana@Example.COM", password: "x".repeat(12) };

test("normalizes email and defaults tags", () => {
  const r = parseSignup(ok);
  assert.strictEqual(r.ok, true);
  assert.strictEqual(r.data.email, "ana@example.com");
  assert.deepStrictEqual(r.data.tags, []);
});
test("exact messages", () => {
  const r = parseSignup({ ...ok, password: "short", age: 12 });
  assert.strictEqual(r.ok, false);
  assert.strictEqual(r.errors.password, "password too short");
  assert.strictEqual(r.errors.age, "too young");
});
test("age must be integer and is optional", () => {
  assert.strictEqual(parseSignup({ ...ok, age: 13.5 }).ok, false);
  assert.strictEqual(parseSignup({ ...ok, age: 13 }).ok, true);
});
test("tags unique non-empty", () => {
  const d = parseSignup({ ...ok, tags: ["a", "a"] });
  assert.strictEqual(d.ok, false);
  assert.strictEqual(d.errors.tags, "duplicate tag");
  assert.strictEqual(parseSignup({ ...ok, tags: [""] }).ok, false);
  assert.strictEqual(parseSignup({ ...ok, tags: ["a", "b"] }).ok, true);
});
test("invalid email", () => {
  const r = parseSignup({ ...ok, email: "nope" });
  assert.strictEqual(r.ok, false);
  assert.ok(typeof r.errors.email === "string");
});
test("schema is a zod schema", () => {
  assert.strictEqual(typeof SignupSchema.safeParse, "function");
});
