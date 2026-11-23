# Schemas

The [v1 catalog](v1/catalog.json) indexes the versioned transaction, receipt, error, ingest and evidence selector schemas and every active operation payload. It lists reserved names for later implementations. [Compatibility promise](../docs/schema-v1.md)

Generate the publication explicitly from the built CLI.

```sh
cargo build --locked -p investigation-cli
python3 scripts/check-v1-schemas.py target/debug/investigation --write
python3 scripts/check-v1-schemas.py target/debug/investigation
```
