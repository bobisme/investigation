# Raw-file hardening fixtures

Hostile inputs for `crates/investigation-store/tests/raw_file_hardening.rs`.
Python's `gzip` and `zipfile` modules generated them with fixed timestamps.

| File | Bytes | SHA-256 | Content |
| --- | --- | --- | --- |
| `nested-bomb.gz` | 269 | `2f6343ed9eee875ad1174737f578d22cdb2c1962f5dcb3779714fbfff98c3eb0` | gzip of a gzip of 65 MiB of zero bytes. The layers expand to 66,279 bytes and then to 68,157,440 bytes, more than a thousand times the stored size. |
| `traversal.zip` | 1,376 | `9659f8dd5e529a45c22c758e5206c9c21cace72e3756ff3c1e064c8f99fed5b4` | Entries `../../escape.txt` (1 MiB of zero bytes), `/tmp/absolute.txt` and a symlink `link` that targets `/etc/passwd`. |

The build links no decompressor or archive reader. The tests check that each
file is retained as its own bytes and that no entry is ever written.
