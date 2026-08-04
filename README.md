# renamite

A motion / vector animation editor built on **Repose**, using **Graphite** geometry
crates, **kurbo** path math, and **lyon** tessellation.

**renamite** owns the UI + GPU device + platform via Repose 0.26.x. Graphite
supplies geometry algorithms only (pinned git, non-WGPU crates). kurbo is the
path math type. The document/animation model is an independent, Glaxnimate-0.6
shaped property/keyframe model (not a Graphene node graph). One shared core
targets both desktop and `wasm32-unknown-unknown`.

## Architecture

```
Repose pointer/keyboard
        │
        ▼
 ToolBehavior reducers  (pure)
        │
        ▼
   EditorCommand stream
        │
        ▼
 renamite-history (transactions / undo)
        │
        ▼
 Serializable Document (renamite-model)
        │
        ▼
 evaluate(frame) -> Scene (display list)
        │
        ▼
 renamite-geometry (kurbo + Graphite facades)
        │
        ▼
 SceneRenderer (lyon tessellation + mesh cache)
        │
 repose-canvas / repose-render-wgpu  (single device)
```

## Workspace layout

```
apps/
  renamite-editor/                 # repose-platform runner (desktop + web + later Android)

crates/
  renamite-geometry/               # VectorPath, kurbo adapters, Graphite (not yet) facades
  renamite-animation/              # Frame, Animated<T>, Tween, Playback, easing
  renamite-model/                  # Document, nodes, eval -> Scene
  renamite-history/                # EditorCommand, transactions, undo/redo
  renamite-behavior-common/        # ToolContext, resolve_property_edit, Selection
  renamite-behavior-canvas/        # Select, Transform, Pen, PathEdit, shapes…
  renamite-behavior-timeline/      # Scrub, Keyframe, EasingCurve
  renamite-render-bridge/          # Scene -> repose DrawCommand / SceneNode
  renamite-ui/                     # Material shell + panels (uses repose-*)
  renamite-io-native/              # .rmot
  renamite-io-lottie/
  renamite-io-svg/
  renamite-io-glax/                # Glaxnimate JSON compatibility
  renamite-machine/                # named clips + state machines → Overrides
  renamite-io-ren/                 # .ren (RON project) + .renb (postcard)
  renamite-platform/               # files, clipboard, autosave backends
  renamite-test-support/           # fixtures, scene snapshots, proptest helpers
```

## Development

```sh
cargo check -p renamite-model -p renamite-animation -p renamite-history -p renamite-machine -p renamite-io-ren --target wasm32-unknown-unknown
cargo test --workspace
cargo build -p renamite-editor --release
```

## Dependency firewall

1. Only `renamite-render-bridge` + `apps/renamite-editor` may depend on `repose-render-wgpu` / `wgpu`.
2. Only `renamite-platform` may depend on `web-sys`, `rfd`, filesystem paths.
3. Pure crates must `cargo check --target wasm32-unknown-unknown` with zero `wasm-bindgen` in their graph.
4. `cargo tree -i wgpu` from pure crates must be empty (guards against Graphite's wgpu 29).

## License

GPL-3.0-or-later.