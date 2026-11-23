# Obsidian

Read-only snapshot rendering and native Bases and Canvas fixtures.

The renderer itself is the `investigation-render` crate in
`crates/investigation-render/`. Its crate documentation describes the
architecture: a pure `RenderPlan` built from one verified snapshot, an
ownership-aware publisher, and a renderer lock separate from the commit lock.
This directory holds vault fixtures; rendering never requires Obsidian to be
installed or running.
