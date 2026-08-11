# renamite

A motion / vector animation editor built on repose, using tesellation via lyon, and kurbo.

The editor supports creating vector primitives, importing image/font assets, editing
layer and property values (with undo), renaming and reordering layers, scrubbing and
basic animation, and exporting frames as PNG/SVG and animation as Lottie. The timeline
is still early (opacity rows only), and there are other rough edges that (might) have to
be handled in repose for completion. kurbo is the path math type. The document/animation
models are independent and can be embedded in other apps (ex: games, via a bevy plugin)

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
 renamite-geometry (kurbo facades)
        │
        ▼
 SceneRenderer (lyon tessellation + mesh cache)
        │
 repose-canvas / repose-render-wgpu  (single device)
```

## Development

```sh
cargo check --target wasm32-unknown-unknown
cargo test --workspace
cargo build -p renamite-editor --release
```

## Try the templates

Built-in starter projects cover the vector primitives in the model. Here is an example that lists them,
creates one, and renders a frame:

```sh
cargo run -p renamite-cli -- templates
cargo run -p renamite-cli -- new demo.ren --template bouncing-ball
cargo run -p renamite-cli -- render demo.ren --frame 0 --out frame.png
```

Available templates: `blank`,
`bouncing-ball`, `loader-trim-path`, `masked-text`, `photo-card`,
`repeater-burst`, `gradient-poster`.


## License

GPL-3.0-or-later.
