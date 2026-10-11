# Licensing

License identifiers follow the [SPDX License List](https://spdx.org/licenses/).

The default license for this project is [AGPL-3.0-only](LICENSE). This applies
to original source code and documentation unless a directory, file, or notice
states otherwise. The complete Desktop, TUI, and hosted Workspace products
continue to be distributed under AGPL-3.0-only.

## Apache-2.0

Original source code and documentation in the following directories and their
subdirectories are licensed under [Apache-2.0](LICENSE-APACHE):

```
packages/core/
packages/model-catalog/
packages/mcp/
packages/runtime_sqlite/
system-skills/skill-creator/
system-skills/skill-installer/
```

The four foundation packages carry their own `LICENSE` for independent
distribution. The two System Skills retain their upstream license, attribution,
and modification notices. These directory exceptions do not replace any
third-party licenses identified below or in individual files.

Product distributions retain both license texts, this scope statement, the
ownership notice, and applicable third-party notices. Paths in this document
identify directories in the source repository.

## Third-party material

Third-party material remains under its stated license:

- Provider logos in `packages/model-catalog/centaeris_model_catalog/logos/`
  retain their [original license and attribution notice](packages/model-catalog/centaeris_model_catalog/logos/LICENSE).
- Bundled fonts in `packages/ui/public/` retain their SIL Open Font License
  notices, indexed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
- Vendored license fallbacks in `packages/desktop/third_party_license_fallbacks/`
  retain their upstream notices. Dependencies retain their own licenses;
  Desktop and TUI distributions include the assembled `THIRD_PARTY_LICENSES`.

## Trademarks

The Centaeris name, logo, and official visual identity are outside these
software licenses. No trademark rights are granted. Third-party marks remain
subject to their owners' rights.
