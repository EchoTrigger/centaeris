# Third-party notices

Office previews bundle Mozilla PDF.js (`pdfjs-dist` 6.3.289), licensed under
Apache-2.0. Its license is distributed with the web application as
[`Apache-2.0-PDFjs.txt`](packages/web/public/licenses/Apache-2.0-PDFjs.txt).
Source: [mozilla/pdf.js](https://github.com/mozilla/pdf.js).

This repository bundles the following fonts in `packages/web`. Vite copies the
linked license files from `packages/web/public/licenses` into the deployed web
artifact under `licenses/`.

| Font | Copyright record | Reference source | License text |
| --- | --- | --- | --- |
| Google Sans Code | Copyright 2025 The Google Sans Code Project Authors | [googlefonts/googlesans-code](https://github.com/googlefonts/googlesans-code) — the exact bundled source revision was not recorded | [`OFL-GoogleSansCode.txt`](packages/web/public/licenses/OFL-GoogleSansCode.txt) |
| Noto Sans CJK SC | The exact copyright string was not separately recorded with the bundled subset files | [notofonts/noto-cjk](https://github.com/notofonts/noto-cjk) — this is the upstream project reference, not an assertion of the exact bundled revision | [`OFL-NotoSansCJK.txt`](packages/web/public/licenses/OFL-NotoSansCJK.txt) |

Both bundled font families are distributed under the SIL Open Font License,
Version 1.1. No unrecorded upstream version or commit is asserted here.
