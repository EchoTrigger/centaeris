---
name: skill-creator
description: Create or update a Centaeris Skill package when the user needs reusable instructions, references, scripts, or assets for a repeatable workflow.
---

<!-- Derived from openai/skills and substantially modified for Centaeris. -->

# Skill Creator

Create the smallest Skill package that makes a recurring workflow reliable.

## Workflow

1. When the matching Core checkout is available, read `docs/reference/SkillPackageSpec.md`; it defines the current package contract.
2. Inspect an existing Skill before changing it. Preserve useful instructions and bundled resources.
3. Keep `SKILL.md` concise. Put detailed domain material in `references/`, deterministic repeated work in `scripts/`, and output inputs in `assets/`.
4. Make the frontmatter `name` match the parent directory exactly. Write `description` as a clear one-line trigger explaining when the model should use the Skill.
5. Link only resources the model should read for a task. Do not duplicate their content in `SKILL.md`.
6. Inspect the resulting package and run the smallest available script or catalog check.

## Create a package

If Python is available and the active execution Host can read this Skill's files, resolve `scripts/create_skill.py` relative to this `SKILL.md`, then run it using that resolved path:

```bash
python "<skill-directory>/scripts/create_skill.py" <name> --catalog <existing-writable-catalog-directory> --description "Use when ..." --with references,scripts
```

The example uses `<skill-directory>` as a placeholder for the actual directory containing this `SKILL.md`; it is not the current workspace directory. Use `python3` when that is the installed command. The native TUI does not bundle Python, so check availability before using the helper.

`--with` accepts a comma-separated subset of `references,scripts,assets`. The helper rejects invalid names and occupied destinations; it never overwrites a package.

If Python or access to the bundled script is unavailable, create the package with the active file tools inside an authorized writable catalog. Use a lower-kebab-case directory name, matching `name`, a one-line `description`, and only the resource directories needed. Check for a destination collision before writing. Creating files alone does not register or activate the Skill.

## Centaeris boundaries

- A Skill supplies instructions. It does not grant tools, permissions, network access, or runtime capabilities.
- Standard package contents are `SKILL.md` plus optional `references/`, `scripts/`, and `assets/`.
- Prefer progressive disclosure: catalog metadata first, `SKILL.md` when selected, referenced files only when needed.
- Do not add `openai.yaml`, `centaeris.yaml`, a second manifest, marketplace metadata, or an installer unless the product contract explicitly requires one.
- Reuse existing runtime tools and execution boundaries. Never describe a tool or path the active environment does not actually provide.
