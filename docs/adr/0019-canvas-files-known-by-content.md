# ADR 0019 – A generated `.canvas` file is known by its content, and written in the viewer's layout

Accepted with bn-24y4; 2026-11-24. Refines plan §12.7 and the ownership rule of the render manifest. Does not change ADR 0016. [Layout and digest](../../crates/investigation-render/src/canvas.rs) · [Publisher](../../crates/investigation-render/src/publish.rs) · [Tests](../../crates/investigation-cli/tests/vault_map.rs)

**Problem.** The real-viewer gate (bn-1br4, Obsidian 1.15.1) opened both scenario case maps. On open, the viewer wrote each `Map.canvas` back in its own JSON layout. After the prefix mapping the content was identical, but the bytes were not. The renderer wrote two-space indentation, spaces after `:` and `,`, and a final line break. The viewer writes tabs, one compact object per line, and no final line break. §12.7 says: "compare its current bytes with the last generated hash. If they differ, leave it in place". The manifest records one SHA-256 per owned path, so the next render reported the map as drifted. The gate's `vault.unchanged` check failed. Anyone who opens a case map would turn it into drift without making any edit.

**Decision.**

1. *Write the viewer's layout.* The renderer writes a `.canvas` file exactly as the viewer writes it:
   - `{`, then each top-level key (`nodes`, then `edges`) on its own line, indented by one tab;
   - each node or edge as one compact `JSON.stringify` object on its own line, indented by two tabs, with lines separated by `,` and a line break;
   - the array closed on its own line, indented by one tab;
   - `}` with no line break after it.

   Characters outside ASCII are written as they are, and `/` is not escaped. Node keys are in the order `id, type, file, x, y, width, height`, and edge keys in the order `id, fromNode, fromSide, toNode, toSide, label`, as the viewer's own files show.

   Both scenario maps the viewer wrote are reproduced byte for byte once the gate's prefix is mapped out. They are checked in as test fixtures. A map opened in the pinned release therefore keeps its bytes, and the manifest's byte hash still matches. The renderer version becomes `inv.vault-renderer.v28`, and the copy `vault copy-map` writes uses the same layout.

2. *Know a `.canvas` file by its content.* The ownership check uses a digest. For a file whose name ends in `.canvas` and whose bytes parse as JSON, the digest is the SHA-256 of decision 1's serialization of the parsed value. Every other file's digest is the SHA-256 of its bytes, unchanged.

   This digest is used wherever the publisher, drift diagnosis, reconcile and `vault clean-conflicts` know a vault file. A file moved aside for verified removal is known by its own name. Since the renderer writes decision 1's serialization, the digest of a file it wrote equals that file's byte hash. The manifest format, the recorded hashes and every file other than a canvas are unchanged.

   Effects:
   - A canvas with the same content in another layout, key order, escaping, or with a final line break has the recorded digest. The publisher treats it as the file it wrote. It is left in place when the plan is unchanged, replaced when the plan changes, and removed when it is retired.
   - A changed value, or an added or removed node, edge or key, is drift, as before.
   - A canvas that is not JSON is known by its bytes, so it is drift.

**Why this is consistent with §12.7.** §12.7 compares bytes to detect a person's edit, so the edit is kept and not overwritten. A viewer that writes the same JSON again has made no edit: no node moved, no text changed, no link changed. Comparing parsed content still catches every edit a person can make in the canvas, including a drag, since that changes `x` or `y`. So nothing that §12.7 protects is lost. Markdown notes and Bases files keep the byte rule. A Markdown re-save that changes only whitespace can still be a person's edit, and there is no single parse that defines Markdown sameness.

**Why not ADR 0016's rule.** ADR 0016 rejects parse-level equivalence for journal files. There the hash is the integrity chain: a second spelling of the same event would give two heads for one history. A vault file's hash is not chained or archived as evidence. It only answers "is this still what the renderer wrote?" For a JSON Canvas file, the content is the answer.

**Rejected alternatives.**

- *Only write the viewer's layout (decision 1 alone).* Another viewer release could pick another layout, and every map would then become drift again. That would happen even after the profile is updated, and nobody would have edited anything.
- *Rewrite the canvas back to our layout on the next render.* The viewer may hold the file open and write it again, so the two writers would trade it back and forth. And a render must not replace a file it cannot prove it owns.
- *Record a second, layout-free digest in the manifest.* Since decision 1 makes the canonical serialization equal to the written bytes, the digest already serves as that second hash, and the manifest format can stay as it is.
- *Enable serde_json's `preserve_order` so a parse keeps the file's key order.* That would change every serialized `Value` in the workspace, including journal events, whose bytes ADR 0016 fixes.

**Consequences.**

- A vault rendered by v27 or earlier re-renders its maps once in the new layout. That is an owned replacement and is not drift.
- Number spelling is part of the content: `400` and `400.0` differ, so a viewer that writes positions as floats would show drift. The pinned release writes integers.
- Only file nodes and labelled edges have been observed from the viewer. The key order for text nodes and other fields follows the JSON Canvas 1.0 field order and has not been seen from the viewer. A different order there costs a byte difference, which decision 2 absorbs. It is never drift.
- A person's canvas under `Boards/` is never owned, so the digest changes nothing about how it is treated; `vault snapshot-note` still captures a person's file by its exact bytes.
