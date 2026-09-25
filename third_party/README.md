# Vendored Protocol Buffer definitions

These files are build inputs kept in the repository so protobuf generation never downloads definitions at build time.

- `google/rpc/status.proto`: copied from `googleapis/googleapis` commit `9f99764b` used by the validated Gate A probe; licensed under Apache-2.0 (`LICENSE.googleapis`).
- `google/protobuf/any.proto`: copied from Protocol Buffers v35.1 inputs used by the validated Gate A probe; licensed under BSD-3-Clause (`LICENSE.protobuf`).

The canonical Qingluan protocol lives under `proto/`; this directory contains external definitions only.
