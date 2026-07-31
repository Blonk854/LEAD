# Contributing to LEAD

Thank you for helping improve LEAD.

LEAD is a fork of [Zed](https://github.com/zed-industries/zed). Upstream community norms and the [Zed Code of Conduct](https://zed.dev/code-of-conduct) are good references when collaborating.

## Contribution ideas

Useful PRs for this fork include:

- Bug fixes and LEAD-specific feature work (agent, Hybrid workers, Unleashed, installer UX, etc.)
- Docs that match how LEAD actually behaves
- Small enhancements and keybindings

For large upstream-style features, the [Zed Feature Process](./docs/src/development/feature-process.md) is still a useful design guide.

## Sending changes

Prefer working code and clear PRs over long discussion threads. Include a short summary of *why* the change exists and how you verified it.

## Developing LEAD

See the development docs:

- [macOS](./docs/src/development/macos.md)
- [Linux](./docs/src/development/linux.md)
- [Windows](./docs/src/development/windows.md)

On Windows, use the LEAD bundle scripts under `script/` (for example `bundle-lead-windows.ps1`) rather than assuming upstream Zed installer paths.
