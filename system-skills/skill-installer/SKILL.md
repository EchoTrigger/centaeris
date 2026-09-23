---
name: skill-installer
description: Register or copy a trusted local Skill package in Centaeris Desktop when the user supplies an exact package or catalog path.
---

<!-- Derived from openai/skills and substantially modified for Centaeris. -->

# Skill Installer

Use only the exact local Skill location supplied by the user. Centaeris does not search conventional directories or fetch remote Skill packages.

## Register in Desktop

When the package can remain where it is, use the Desktop Skills panel to register its exact catalog directory or `SKILL.md`. Choose Workspace scope for one exact local workspace or User scope for all local workspaces. Registration changes configuration; it does not copy files or grant tools, permissions, network access, or runtime capabilities.

The native TUI currently has no Skill source management command. Hosted Workspace discovers its built-in System Skills and Skills from activated Plugins; it does not register an arbitrary local Skill path. For those Hosts, explain the available Host-specific installation path or the missing capability. Do not claim that a copied directory is active.

## Make a local copy for Desktop

When the user explicitly requests an owned copy, first check that Python is available and the destination is writable. Resolve `scripts/install_skill.py` relative to this `SKILL.md`, then run:

```bash
python "<skill-directory>/scripts/install_skill.py" <source-skill-directory> --catalog <existing-writable-catalog-directory>
```

The example uses `<skill-directory>` as a placeholder for the actual directory containing this `SKILL.md`; it is not the current workspace directory. Use `python3` when that is the installed command. The native TUI does not bundle Python.

The helper checks the package name and required frontmatter, rejects links or reparse points within the source and an occupied destination, then copies the package through staging and an atomic rename. It never downloads, updates, overwrites, or changes Centaeris configuration. Core's catalog validation remains authoritative.

After copying, register the destination catalog through the Desktop Skills panel. Do not edit the Runtime's user configuration file directly.

## Boundaries

- Accept only a local package the user has identified and trusts.
- Never infer a source path or silently fall back to Git, GitHub, npm, pip, a marketplace, or another catalog.
- Do not overwrite or merge an existing package. Stop and report the exact collision.
- Do not report installation complete until the receiving Host's catalog shows the Skill.
