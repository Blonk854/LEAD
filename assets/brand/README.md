# LEAD brand assets

## App icon

Vector source: `lead-icon.svg`. Raster master: `lead-app-icon-master.png` (1024×1024),
exported from the SVG. A yellow pencil sits on a dark-blue squircle.

The dev channel uses `lead-app-icon-master-dev.png`, which adds a yellow **DEV** pill badge
(bottom-right, generated automatically).

Regenerate all bundled PNG/ICO sizes after editing the SVG/master:

```powershell
# If you changed lead-icon.svg, rasterize to the master PNG first (resvg / similar), then:
python script/export-lead-app-icons.py
```

Outputs:

- `crates/zed/resources/app-icon*.png` (512 and 1024 @2x, all release channels)
- `crates/zed/resources/windows/app-icon*.ico`
- `inno/lead-x86_64/app-icon*.ico`

Rebuild LEAD to pick up embedded icons: `cargo build -p zed --bin lead`

## In-app UI icons (Phase B)

The following files in `assets/icons/` were redesigned with a pencil motif while keeping
existing `IconName` identifiers (`ZedAgent`, etc.) for compatibility:

- `zed_agent.svg`, `zed_agent_two.svg`, `zed_assistant.svg`
- `ai_zed.svg`
- `zed_predict*.svg` (5 variants)
- `zed_src_custom.svg`, `zed_src_extension.svg`

UI icons stay monochrome (black strokes) so theme tinting works; color lives in the app icon only.
