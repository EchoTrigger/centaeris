# Electron shell

This dormant package preserves the native window lifecycle, renderer isolation,
sender validation and Windows packaging defaults for a future desktop product.
It has its own pnpm workspace and lockfile. The root install, CI and release
jobs do not build or install it.

A future application supplies its renderer, preload and IPC handlers, app
identity, resources and runtime client. The shell has no product entry point.
Packaged windows load a local renderer. Development can opt into an exact
loopback renderer URL. Packaging can extend `electron-builder.base.mjs`.
For distribution notices, see the shared [license helper](../../scripts/third-party-licenses.mjs).
