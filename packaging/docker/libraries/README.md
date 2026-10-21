# Making your own SolidCraft libraries

A library is a file of looks (appearances) and physical materials that every browser on this
server can use. The web app reads every `*.solidcraft-library.json` in the designs folder's
`Libraries` subfolder when a page loads, and the Appearance window lists them under
**Local library**. This file can sit in that folder too: the app only reads library files.

## The easy way: save one from the app

1. Open a design (a scratch one is fine).
2. Make the looks you want to keep:
   - **Appearance** on a body: pick a colour, and optionally a surface response (metal or not,
     how rough) and a **Pattern** (wood grain, brushed, blasted, mottled, speckled, veined).
   - Or a **physical material** with a density and a default look, so parts get the right mass
     as well as the right look.
3. In the Appearance window, under **Local library**, type a name in **Save as library** and press
   **Save to the server**. Tick **Replace existing library file** to update one you saved before.
4. Everyone gets it the next time their page loads.

Only appearances that are actually on something, and materials made in that design, are saved.

## By hand

Write a text file whose name ends in `.solidcraft-library.json`, put it in the `Libraries` folder,
and reload the page. If something in it is wrong, picking it in the Appearance window says what.

```json
{
  "format": "solidcraft-library/2",
  "id": "my.stones",
  "name": "My stones",
  "entries": [
    {
      "definition": {
        "kind": "appearance",
        "value": {
          "name": "Stone - Grey granite",
          "color": [150, 148, 146],
          "pattern": { "kind": "speckled", "scale_mm": 2.5, "contrast": 0.55, "color": [52, 50, 52] }
        }
      }
    },
    {
      "definition": {
        "kind": "material",
        "value": {
          "name": "Bluestone",
          "density_g_cm3": 2.6,
          "appearance": {
            "name": "Stone - Bluestone",
            "color": [92, 104, 116],
            "pbr": { "metallic": 0.0, "roughness": 0.6, "dielectric_ior": 1.5 },
            "pattern": { "kind": "mottled", "scale_mm": 20, "contrast": 0.3, "color": [70, 80, 92] }
          }
        }
      }
    }
  ]
}
```

### The file

| Field | What it is |
| --- | --- |
| `format` | `"solidcraft-library/2"` (use `/1` only for colour-only looks, which plain upstream builds can read) |
| `id` | The library's id: lowercase letters, digits, `.`, `_` and `-`, starting with a letter, at most 64 characters, different for every library |
| `name` | What the Appearance window shows, at most 128 characters |
| `entries` | The looks and materials, at most 1024 |

Each entry is `{"definition": {"kind": "appearance" or "material", "value": {…}}}`. Saved files
also have `id`, `revision` and `provenance` on each entry. Leave those out when you write one by
hand: they are filled in when the file is read. If you edit an entry that has them, delete its
`id` and `revision` lines, or the file is refused: the revision is a fingerprint of the entry, and
it no longer matches.

### An appearance

| Field | What it is |
| --- | --- |
| `name` | Its name; `"Group - Variant"` names group nicely (`"Stone - Slate"`) |
| `color` | `[red, green, blue]`, each 0–255 |
| `opacity` | 0–1, optional (1, opaque, when left out) |
| `pbr` | Optional surface response for the Render workspace: `metallic` 0–1 (1 for bare metal), `roughness` 0.045–1 (low: polished), `dielectric_ior` 1–3 (1.5 suits almost everything). Needs `opacity` 1 |
| `pattern` | Optional surface pattern, below |

### A physical material

| Field | What it is |
| --- | --- |
| `name` | Its name. Not one of the built-in materials: Default, Steel, Stainless Steel, Aluminum, Brass, Copper, Titanium, Cast Iron, ABS Plastic, PLA, Nylon, Polycarbonate, Oak, Glass |
| `density_g_cm3` | Density in g/cm³, above 0 and at most 1000 (water is 1, steel about 7.85) |
| `appearance` | Optional default look, as above |

### A pattern

| Field | What it is |
| --- | --- |
| `kind` | `wood`, `brushed`, `blasted`, `mottled`, `speckled` or `veined` |
| `scale_mm` | 0.25–60 mm; what it means depends on the kind (below) |
| `contrast` | 0–1: how strongly the pattern shows (for `speckled` and `mottled`, also how much of the second colour) |
| `color` | The second colour, `[red, green, blue]`: latewood, patina, rust, grains, veins |
| `axis` | Optional: `auto` (the part's longest side, the default), `x`, `y` or `z`; the direction of grain, brushing and veins |
| `seed` | Optional 0–255: another variation of the same pattern |

| Kind | Draws | `scale_mm` | Good for |
| --- | --- | --- | --- |
| `wood` | growth rings, early and late wood, fibres along the grain; rings on the end grain | ring spacing (2–7) | timber, plywood |
| `brushed` | long thin streaks along the axis | streak width (0.3–1) | brushed or ground metal |
| `blasted` | an even fine speckle | speckle size (0.3–1) | bead blasting, anodizing, powder coat, MDF |
| `mottled` | soft patches of the second colour | patch size (10–40) | patina, rust, mill scale, slate, weathering |
| `speckled` | grains of the second colour with lighter flecks | grain size (1–4) | concrete, granite, terrazzo, sandstone, brick |
| `veined` | thin winding veins of the second colour | vein spacing (30–50) | marble, travertine |

Patterns show in the Design view, on opaque parts, where the GPU runs compute shaders (WebGPU in the
browser; the desktop app on most graphics cards). Elsewhere, and in the Render workspace, parts show
their colour.

## The Workshop Materials library

`workshop-materials.solidcraft-library.json` is made by a small program
(`W:\scripts\solidcraft-library`: edit its tables, then `cargo run --release -- <output.json>`).
You can also edit it by hand as described above. Its densities and colours are typical values for
visualisation and mass estimates, not graded engineering data.
