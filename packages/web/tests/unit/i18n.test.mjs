import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { createInstance } from "i18next";

const read = (language) => JSON.parse(readFileSync(new URL(`../../src/locales/${language}.json`, import.meta.url), "utf8"));
const en = read("en");
const zh = read("zh-CN");
test("both languages cover the same keys and interpolation parameters", () => {
  assert.deepEqual(Object.keys(en).sort(), Object.keys(zh).sort());
  const params = (text) => [...text.matchAll(/{{\s*(\w+)\s*}}/g)].map((m) => m[1]).sort();
  for (const key of Object.keys(en)) {
    assert.ok(en[key].trim(), key);
    assert.ok(zh[key].trim(), key);
    assert.deepEqual(params(en[key]), params(zh[key]), key);
    assert.doesNotMatch(en[key], /\p{Script=Han}/u, key);
  }
});
test("counts use English plurals and preserve interpolated content", async () => {
  const instance = createInstance();
  await instance.init({ resources: { en: { translation: en }, "zh-CN": { translation: zh } }, lng: "en", keySeparator: false, interpolation: { escapeValue: false } });
  assert.equal(instance.t("models.count", { count: 1 }), "1 model");
  assert.equal(instance.t("models.count", { count: 2 }), "2 models");
  await instance.changeLanguage("zh-CN");
  assert.equal(instance.t("models.count", { count: 2 }), "2 个模型");
  assert.equal(instance.t("invitation.join", { workspace: "Research <&> 研究" }), "加入 Research <&> 研究");
});
