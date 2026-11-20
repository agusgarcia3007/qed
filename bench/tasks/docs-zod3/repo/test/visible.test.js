const test = require("node:test");
const assert = require("node:assert");
const { parseSignup } = require("../user.js");

test("valid signup", () => {
  const r = parseSignup({ email: "a@b.co", password: "x".repeat(12) });
  assert.strictEqual(r.ok, true);
});
